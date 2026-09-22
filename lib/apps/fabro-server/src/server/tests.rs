use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc as StdArc, Mutex as StdMutex};

use axum::body::Body;
use axum::http::{Method, Request, header};
use chrono::{Duration as ChronoDuration, Utc};
use fabro_automation::AutomationId;
use fabro_config::bind::Bind;
use fabro_config::{LlmLayer, RunLayer, ServerSettingsBuilder};
use fabro_interview::{
    AnswerValue, WorkerControlAck, WorkerControlDeliveryFrame, WorkerControlEnvelope,
    WorkerControlMessage, WorkerControlOutcome,
};
use fabro_llm::lithos_catalog::Catalog;
use fabro_store::platform_records::{
    PlatformRecord, RunLifecycleKind, RunLifecycleRecord, StoredPlatformRecord,
};
use fabro_types::settings::ServerAuthMethod;
use fabro_types::settings::run::{ApprovalMode, RunMode};
use fabro_types::{
    AuthMethod, BlobHash, GitRunTarget, InterviewQuestionRecord, ModelRef, QuestionType, RunId,
    RunTarget, SandboxProviderKind, SuccessReason, SystemActorKind, fixtures,
};
use fabro_util::check_report::CheckStatus;
use httpmock::Method::{GET, POST};
use httpmock::MockServer;
use lithos_llm::catalog::ModelId;
use lithos_llm::types::{Cost, CostSource, Request as LlmRequest, Speed, TokenCounts};
use pebble_coding_agent::events::Usage;
use serde_json::json;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message as WebSocketMessage;
use tower::ServiceExt;
use tracing::field::{Field, Visit};
use tracing::{Event as TracingEvent, Subscriber, subscriber};
use tracing_subscriber::layer::Context as SubscriberContext;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{Layer, Registry};
use ulid::Ulid;

use super::*;
use crate::automation_materializer::AutomationRunMaterializeInput;
use crate::github_webhooks::compute_signature;
use crate::jwt_auth::{AuthMode, ConfiguredAuth};
use crate::test_support::*;
use crate::worker_control::{
    LocalWorkerControlBus, WorkerControlAcks, WorkerControlBus, WorkerControlCursor,
    WorkerControlReceiver,
};
use crate::worker_runtime::{
    LocalWorkerRuntime, StartedWorker, WorkerLaunchSpec, WorkerRef, WorkerRuntime,
};

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

fn test_environment_store(
    default_provider: Option<SandboxProviderKind>,
    local_enabled: bool,
) -> (tempfile::TempDir, EnvironmentStore) {
    let temp = tempfile::tempdir().expect("environment store tempdir should be created");
    let db_path = temp.path().join("fabro.sqlite3");
    let pool = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("environment store setup runtime should build");
        runtime.block_on(async move {
            let database = fabro_db::Database::connect(db_path)
                .await
                .expect("test environment database should connect");
            database
                .migrate()
                .await
                .expect("test environment database should migrate");
            if let Some(provider) = default_provider {
                fabro_environment::seed_default_environment(database.pool(), provider)
                    .await
                    .expect("test default environment should seed");
            }
            database.clone_pool()
        })
    })
    .join()
    .expect("environment store setup thread should not panic");
    let store = load_store_blocking("environment store", move || async move {
        EnvironmentStore::load(pool, local_enabled)
            .await
            .map_err(anyhow::Error::new)
    })
    .expect("test environment store should load");
    (temp, store)
}

fn test_mcp_server_store() -> (tempfile::TempDir, McpServerStore) {
    let temp = tempfile::tempdir().expect("MCP server store tempdir should be created");
    let db_path = temp.path().join("fabro.sqlite3");
    let pool = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("MCP server store setup runtime should build");
        runtime.block_on(async move {
            let database = fabro_db::Database::connect(db_path)
                .await
                .expect("test MCP server database should connect");
            database
                .migrate()
                .await
                .expect("test MCP server database should migrate");
            database.clone_pool()
        })
    })
    .join()
    .expect("MCP server store setup thread should not panic");
    let mcps_dir = temp.path().join("mcps");
    let store = load_store_blocking("MCP server store", move || async move {
        McpServerStore::open(pool, mcps_dir)
            .await
            .map_err(anyhow::Error::new)
    })
    .expect("test MCP server store should load");
    (temp, store)
}

fn server_settings_from_toml(source: &str) -> ServerSettings {
    ServerSettingsBuilder::from_toml(source).expect("server settings should resolve")
}

fn resolved_runtime_settings_from_toml(source: &str) -> ResolvedAppStateSettings {
    resolved_runtime_settings_for_tests(
        server_settings_from_toml(source),
        manifest_run_defaults_from_toml(source),
        LlmLayer::default(),
    )
}

fn test_app_with() -> Router {
    let state = test_app_state();
    crate::test_support::build_test_router_with_options(state, RouterOptions {
        static_asset_root: Some(spa_fixture_root()),
        ..RouterOptions::default()
    })
}

fn spa_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/spa")
}

fn state_test_catalog() -> Arc<Catalog> {
    Arc::new(fabro_llm::test_support::test_catalog())
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

/// An operator-defined OpenAI-compatible provider `acme` offering one model,
/// `acme-large`, with `credential` (`env:NAME` or `vault:NAME`).
/// An operator-defined provider. Its API key is `ACME_API_KEY`, the name
/// lithos derives from the provider id, whether it lives in the vault or the
/// environment.
fn acme_overlay(base_url: &str) -> String {
    format!(
        r#"
[providers.acme]
display_name = "Acme"
base_url = {base_url}
auth = {{ type = "bearer" }}
priority = 120
default_model = "acme-large"

[providers.acme.metadata.agent]
profile = "openai"

[providers.acme.models."acme-large"]
display_name = "Acme Large"
api_model = "acme-large"
limits = {{ context_tokens = 128000, max_output_tokens = 8192 }}
capabilities = {{ text = true, tools = true }}
probe = true

"#,
        base_url = toml::Value::String(base_url.to_string()),
    )
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
    let state = TestAppStateBuilder::new()
        .env_lookup(|_| None)
        .vault_entries([(WEBHOOK_SECRET_ENV, TEST_WEBHOOK_SECRET)])
        .build();
    build_router_with_options(state, &auth_mode, RouterOptions {
        web_enabled: false,
        ..RouterOptions::default()
    })
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
                .body(Body::from(
                    test_intent_with_bearer(app, "workflow.fabro", MINIMAL_DOT, None, Some(bearer))
                        .await
                        .to_string(),
                ))
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

struct WorkerControlWsTestServer {
    base_url: String,
    task:     tokio::task::JoinHandle<()>,
}

impl WorkerControlWsTestServer {
    async fn spawn(app: Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test WebSocket listener should bind");
        let addr = listener
            .local_addr()
            .expect("test WebSocket listener should have a local address");
        let task = tokio::spawn(async move {
            let result = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await;
            if let Err(err) = result {
                tracing::debug!(error = %err, "test WebSocket server stopped");
            }
        });
        Self {
            base_url: format!("ws://{addr}"),
            task,
        }
    }

    fn worker_control_url(&self, run_id: RunId, after: Option<&str>) -> String {
        let mut url = format!(
            "{}/api/v1/runs/{run_id}/worker/control-stream",
            self.base_url
        );
        if let Some(after) = after {
            url.push_str("?after=");
            url.push_str(after);
        }
        url
    }
}

impl Drop for WorkerControlWsTestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn worker_control_ws_request(
    server: &WorkerControlWsTestServer,
    run_id: RunId,
    bearer: Option<&str>,
    after: Option<&str>,
) -> Request<()> {
    let mut request = server
        .worker_control_url(run_id, after)
        .into_client_request()
        .expect("test worker-control WebSocket request should build");
    if let Some(bearer) = bearer {
        request.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {bearer}")
                .parse()
                .expect("test bearer header should parse"),
        );
    }
    request
}

async fn connect_worker_control_ws(
    server: &WorkerControlWsTestServer,
    run_id: RunId,
    bearer: &str,
    after: Option<&str>,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let request = worker_control_ws_request(server, run_id, Some(bearer), after);
    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("worker-control WebSocket should connect");
    socket
}

async fn assert_worker_control_ws_rejected(
    server: &WorkerControlWsTestServer,
    run_id: RunId,
    bearer: Option<&str>,
    after: Option<&str>,
    expected: StatusCode,
) {
    let request = worker_control_ws_request(server, run_id, bearer, after);
    let error = tokio_tungstenite::connect_async(request)
        .await
        .expect_err("worker-control WebSocket should be rejected");
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), expected);
        }
        other => panic!("expected HTTP rejection {expected}, got {other:#}"),
    }
}

async fn next_worker_control_frame(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> WorkerControlDeliveryFrame {
    let message = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let message = futures_util::StreamExt::next(socket)
                .await
                .expect("worker-control WebSocket should remain open")
                .expect("worker-control WebSocket frame should be ok");
            match message {
                WebSocketMessage::Text(text) => return text,
                WebSocketMessage::Ping(payload) => {
                    futures_util::SinkExt::send(socket, WebSocketMessage::Pong(payload))
                        .await
                        .expect("test worker-control pong should send");
                }
                WebSocketMessage::Pong(_)
                | WebSocketMessage::Binary(_)
                | WebSocketMessage::Frame(_) => {}
                WebSocketMessage::Close(frame) => {
                    panic!("worker-control WebSocket closed before text frame: {frame:?}");
                }
            }
        }
    })
    .await
    .expect("worker-control frame should arrive");
    serde_json::from_str(message.as_str()).expect("worker-control delivery frame should parse")
}

#[tokio::test(flavor = "current_thread")]
async fn worker_control_stream_rejects_missing_user_and_cross_run_auth() {
    let (_state, app) = jwt_auth_app();
    let user_bearer = issue_test_user_jwt();
    let run_id = create_run_with_bearer(&app, &user_bearer).await;
    let worker_bearer = issue_test_worker_token(&run_id);
    let other_run_id = create_run_with_bearer(&app, &user_bearer).await;
    let other_worker_bearer = issue_test_worker_token(&other_run_id);
    let server = WorkerControlWsTestServer::spawn(app).await;

    assert_worker_control_ws_rejected(&server, run_id, None, None, StatusCode::UNAUTHORIZED).await;
    assert_worker_control_ws_rejected(
        &server,
        run_id,
        Some(&user_bearer),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_worker_control_ws_rejected(
        &server,
        run_id,
        Some(&other_worker_bearer),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;

    let mut socket = connect_worker_control_ws(&server, run_id, &worker_bearer, None).await;
    futures_util::SinkExt::send(&mut socket, WebSocketMessage::Close(None))
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn worker_control_stream_start_subscription_delivers_frames() {
    let (state, app) = jwt_auth_app();
    let user_bearer = issue_test_user_jwt();
    let run_id = create_run_with_bearer(&app, &user_bearer).await;
    let worker_bearer = issue_test_worker_token(&run_id);
    let server = WorkerControlWsTestServer::spawn(app).await;
    let mut socket = connect_worker_control_ws(&server, run_id, &worker_bearer, None).await;

    let expected = WorkerControlEnvelope::cancel_run();
    let id = state
        .worker_control_bus
        .publish(run_id, expected.clone())
        .await
        .unwrap();
    let frame = next_worker_control_frame(&mut socket).await;

    assert_eq!(frame.id, id.to_string());
    assert_eq!(frame.envelope, expected);
}

#[tokio::test(flavor = "current_thread")]
async fn worker_control_stream_after_subscription_delivers_only_later_frames() {
    let (state, app) = jwt_auth_app();
    let user_bearer = issue_test_user_jwt();
    let run_id = create_run_with_bearer(&app, &user_bearer).await;
    let worker_bearer = issue_test_worker_token(&run_id);
    let first = state
        .worker_control_bus
        .publish(run_id, WorkerControlEnvelope::cancel_run())
        .await
        .unwrap();
    let expected = WorkerControlEnvelope::pause_run();
    let second = state
        .worker_control_bus
        .publish(run_id, expected.clone())
        .await
        .unwrap();
    let server = WorkerControlWsTestServer::spawn(app).await;

    let mut socket =
        connect_worker_control_ws(&server, run_id, &worker_bearer, Some(first.as_str())).await;
    let frame = next_worker_control_frame(&mut socket).await;

    assert_eq!(frame.id, second.to_string());
    assert_eq!(frame.envelope, expected);
}

/// An acknowledgement the worker sends over its control stream settles the
/// caller waiting on that request; one for another run's request does
/// not.
#[tokio::test(flavor = "current_thread")]
async fn worker_control_stream_acknowledgements_settle_the_waiting_caller() {
    let (state, app) = jwt_auth_app();
    let user_bearer = issue_test_user_jwt();
    let run_id = create_run_with_bearer(&app, &user_bearer).await;
    let worker_bearer = issue_test_worker_token(&run_id);
    let server = WorkerControlWsTestServer::spawn(app).await;
    let mut socket = connect_worker_control_ws(&server, run_id, &worker_bearer, None).await;

    let pending = state.worker_control_acks.register(run_id);
    let foreign = state.worker_control_acks.register(fixtures::RUN_2);
    for request_id in [&foreign.request_id, &pending.request_id] {
        let ack = WorkerControlAck::new(request_id, WorkerControlOutcome::Refused {
            code:    "no_live_turn".to_string(),
            message: "the stage has no model turn to interrupt".to_string(),
        });
        futures_util::SinkExt::send(
            &mut socket,
            WebSocketMessage::Text(serde_json::to_string(&ack).unwrap().into()),
        )
        .await
        .expect("the acknowledgement sends");
    }

    let outcome = tokio::time::timeout(
        Duration::from_secs(2),
        state.worker_control_acks.wait(pending),
    )
    .await
    .expect("the caller is answered");
    assert_eq!(
        outcome,
        Some(WorkerControlOutcome::Refused {
            code:    "no_live_turn".to_string(),
            message: "the stage has no model turn to interrupt".to_string(),
        })
    );
    // The other run's request was not this worker's to answer.
    assert_eq!(state.worker_control_acks.outstanding(), 1);
    drop(foreign);
}

#[tokio::test(flavor = "current_thread")]
async fn worker_control_stream_invalid_cursor_is_http_gone_before_upgrade() {
    let (_state, app) = jwt_auth_app();
    let user_bearer = issue_test_user_jwt();
    let run_id = create_run_with_bearer(&app, &user_bearer).await;
    let worker_bearer = issue_test_worker_token(&run_id);
    let server = WorkerControlWsTestServer::spawn(app).await;

    assert_worker_control_ws_rejected(
        &server,
        run_id,
        Some(&worker_bearer),
        Some("local:999"),
        StatusCode::GONE,
    )
    .await;
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

fn json_request(method: Method, path: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(api(path))
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
    crate::test_support::build_test_router_with_options(state, RouterOptions::default())
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
        // No FABRO_WEB_URL override: web.url is plain config now, so the
        // invalid value is rejected from the settings literal and the kept
        // previous settings stay valid.
        let state = test_app_state_with_env_lookup(
            canonical_origin_settings("http://valid.example.com"),
            RunLayer::default(),
            5,
            |_| None,
        );

        let err = state
            .replace_runtime_settings(resolved_runtime_settings_from_toml(&format!(
                r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.web]
url = "{invalid}"
"#,
            )))
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
fn canonical_origin_prefers_fabro_web_url_env_override() {
    // FABRO_WEB_URL is the native control-plane override; it wins over the
    // plain `server.web.url` settings literal.
    let state = test_app_state_with_env_lookup(
        canonical_origin_settings("http://settings.example.com"),
        RunLayer::default(),
        5,
        |name| (name == "FABRO_WEB_URL").then(|| "http://env.example.com".to_string()),
    );

    assert_eq!(state.canonical_origin().unwrap(), "http://env.example.com");
}

#[test]
fn canonical_origin_uses_settings_literal_without_env_override() {
    // Without FABRO_WEB_URL set, the plain `server.web.url` literal is used.
    let state = test_app_state_with_env_lookup(
        canonical_origin_settings("http://settings.example.com"),
        RunLayer::default(),
        5,
        |_| None,
    );

    assert_eq!(
        state.canonical_origin().unwrap(),
        "http://settings.example.com"
    );
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
    assert_eq!(state.server_settings().server.storage.root, "/srv/new");
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
    let (_environment_temp, environment_store) =
        test_environment_store(Some(SandboxProviderKind::DAYTONA), true);
    let (_mcp_temp, mcp_server_store) = test_mcp_server_store();
    let source = r#"
_version = 1

[run.environment]
id = "default"
"#;
    let manifest_run_settings = resolve_manifest_run_settings_with_catalog(
        &run_manifest::manifest_run_defaults(Some(&manifest_run_defaults_from_toml(source))),
        &environment_store,
        &mcp_server_store,
    );

    assert_eq!(system_sandbox_provider(&manifest_run_settings), "daytona");
}

#[test]
fn system_sandbox_provider_defaults_when_manifest_run_settings_do_not_resolve() {
    let (_environment_temp, environment_store) = test_environment_store(None, true);
    let (_mcp_temp, mcp_server_store) = test_mcp_server_store();
    let source = r#"
_version = 1

[run.environment]
id = "missing"
"#;
    let manifest_run_settings = resolve_manifest_run_settings_with_catalog(
        &run_manifest::manifest_run_defaults(Some(&manifest_run_defaults_from_toml(source))),
        &environment_store,
        &mcp_server_store,
    );

    assert_eq!(
        system_sandbox_provider(&manifest_run_settings),
        SandboxProviderKind::default().to_string()
    );
}

#[test]
fn sandbox_provider_policy_error_reports_disabled_provider() {
    let settings = server_settings_from_toml(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.sandbox.providers.daytona]
enabled = false
"#,
    );

    assert_eq!(
        crate::run_manifest::sandbox_provider_policy_error(
            &settings,
            &SandboxProviderKind::DAYTONA
        )
        .as_deref(),
        Some(
            "sandbox provider \"daytona\" is disabled by server.sandbox.providers.daytona.enabled"
        )
    );
}

#[test]
fn clone_sandbox_credentials_are_available_for_clone_based_providers() {
    use fabro_types::SandboxProviderKind;
    assert!(SandboxProviderKind::DOCKER.clones_workspace());
    assert!(SandboxProviderKind::DAYTONA.clones_workspace());
    assert!(!SandboxProviderKind::LOCAL.clones_workspace());
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

    let vault = state.stores.vault.snapshot().await.unwrap();
    assert_eq!(
        vault.get_entry("/tmp/test.pem").unwrap().secret_type,
        SecretType::File
    );
    assert_eq!(vault.file_secrets(), vec![(
        "/tmp/test.pem".to_string(),
        "pem-data".to_string()
    )]);
}

fn create_token_secret_request(name: &str, value: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(api("/secrets"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "name": name,
                "value": value,
                "type": "token"
            }))
            .unwrap(),
        ))
        .unwrap()
}

#[tokio::test]
async fn create_secret_rejects_bootstrap_secret_names() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    for name in [EnvVars::SESSION_SECRET, EnvVars::FABRO_DEV_TOKEN] {
        let response = app
            .clone()
            .oneshot(create_token_secret_request(name, "secret-value"))
            .await
            .unwrap();
        let body = response_json!(response, StatusCode::BAD_REQUEST).await;

        assert_eq!(
            body["errors"][0]["detail"],
            format!("{name} is a bootstrap secret; configure it with process env or server.env")
        );
        assert!(state.stores.vault.get(name).await.unwrap().is_none());
    }
}

#[tokio::test]
async fn create_secret_allows_optional_vault_and_custom_secret_names() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    for (name, value) in [
        (EnvVars::GITHUB_APP_CLIENT_SECRET, "github-client-secret"),
        ("CUSTOM_WORKFLOW_TOKEN", "custom-secret"),
    ] {
        let response = app
            .clone()
            .oneshot(create_token_secret_request(name, value))
            .await
            .unwrap();

        assert_status!(response, StatusCode::OK).await;
        assert_eq!(
            state
                .stores
                .vault
                .get(name)
                .await
                .unwrap()
                .map(|entry| entry.value),
            Some(value.to_string())
        );
    }
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
    let state = TestAppStateBuilder::new().build();
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
    let listed = state.stores.vault.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "OPENAI_CODEX");
    assert_eq!(listed[0].secret_type, SecretType::Oauth);
    assert!(
        state
            .stores
            .vault
            .get("OPENAI_CODEX")
            .await
            .unwrap()
            .is_some()
    );
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
        .stores
        .vault
        .set(
            EnvVars::DAYTONA_API_KEY,
            "existing",
            SecretType::Token,
            None,
        )
        .await
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
        "Daytona API key is missing required scopes: write:snapshots, write:sandboxes. \
         Regenerate the key with all snapshot and sandbox scopes."
    );
    assert_eq!(
        state
            .stores
            .vault
            .get(EnvVars::DAYTONA_API_KEY)
            .await
            .unwrap()
            .map(|entry| entry.value),
        Some("existing".to_string())
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
    let settings = fabro_config::ServerSettingsBuilder::from_toml(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.sandbox.providers.docker]
enabled = false
"#,
    )
    .expect("settings should parse");
    let state = test_app_state_with_env_lookup(
        settings,
        fabro_config::RunLayer::default(),
        5,
        move |name| match name {
            EnvVars::DAYTONA_API_URL => Some(base_url.clone()),
            _ => None,
        },
    );
    state
        .stores
        .vault
        .set(
            EnvVars::DAYTONA_API_KEY,
            "dtn_test",
            SecretType::Token,
            None,
        )
        .await
        .unwrap();

    let report = crate::diagnostics::run_all(&state).await;
    let cloud_sandbox = report
        .sections
        .iter()
        .flat_map(|section| &section.checks)
        .find(|check| check.name == "Cloud Sandbox")
        .expect("cloud sandbox check should be present");

    assert_eq!(cloud_sandbox.status, CheckStatus::Error);
    assert_eq!(
        cloud_sandbox.summary,
        "Daytona API key is missing required scopes"
    );
    assert_eq!(
        cloud_sandbox.details[0].text,
        "missing: write:snapshots, write:sandboxes"
    );
    assert_eq!(
        cloud_sandbox.remediation.as_deref(),
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
        .stores
        .vault
        .set(
            "OPENAI_API_KEY",
            "vault-openai-key",
            SecretType::Token,
            None,
        )
        .await
        .unwrap();

    let llm_result = state.resolve_llm_client().await.unwrap();

    assert_eq!(llm_result.provider_ids(), vec![
        lithos_llm::catalog::builtin::openai()
    ]);
    assert!(llm_result.auth_issues.is_empty());
}

#[tokio::test]
async fn resolve_llm_client_ignores_env_lookup_provider_tokens() {
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        |name| (name == EnvVars::OPENAI_API_KEY).then(|| "env-openai-key".to_string()),
    );

    let llm_result = state.resolve_llm_client().await.unwrap();

    assert!(
        llm_result.provider_ids().is_empty(),
        "server LLM credentials should come from vault only"
    );
    assert!(llm_result.auth_issues.is_empty());
}

struct FailingCredentialSource;

#[async_trait::async_trait]
impl CredentialProvider for FailingCredentialSource {
    async fn credentials(
        &self,
        provider: &fabro_llm::lithos_catalog::CatalogProvider,
    ) -> Result<fabro_llm::credentials::Credentials, fabro_llm::credentials::CredentialError> {
        Err(fabro_llm::credentials::CredentialError::NotConfigured {
            provider: provider.id().clone(),
        })
    }

    async fn is_configured(&self, _provider: &fabro_llm::lithos_catalog::CatalogProvider) -> bool {
        false
    }
}

#[tokio::test]
async fn resolve_llm_client_from_source_with_no_credentials_has_no_ready_providers() {
    let catalog = state_test_catalog();
    let built = resolve_llm_client_from_source(Arc::new(FailingCredentialSource), catalog, None)
        .await
        .expect("a client with no credentials still builds");

    assert!(built.ready.is_empty());
    assert!(built.auth_issues.is_empty());
    assert!(built.provider_ids().is_empty());
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
        .stores
        .vault
        .set(
            "OPENAI_API_KEY",
            "vault-openai-key",
            SecretType::Token,
            None,
        )
        .await
        .unwrap();

    assert_eq!(state.configured_llm_provider_ids().await, vec![
        lithos_llm::catalog::builtin::openai()
    ]);
}

#[tokio::test]
async fn resolve_llm_client_uses_vault_key_without_env_lookup_openai_settings() {
    let server = MockServer::start_async().await;
    let response_mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/responses")
                .header("authorization", "Bearer vault-openai-key");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(openai_responses_payload("hello from vault key"));
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
        .stores
        .vault
        .set(
            "OPENAI_API_KEY",
            "vault-openai-key",
            SecretType::Token,
            None,
        )
        .await
        .unwrap();

    let llm_result = state.resolve_llm_client().await.unwrap();
    let response = llm_result
        .client
        .complete(
            LlmRequest::builder()
                .model("openai/gpt-5.4")
                .user("Hello")
                .build()
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.text(), "hello from vault key");
    response_mock.assert_async().await;
}

#[tokio::test]
async fn list_secrets_includes_oauth_metadata() {
    let state = test_app_state();
    state
        .stores
        .vault
        .set(
            "OPENAI_CODEX",
            &openai_oauth_credential_json(),
            SecretType::Oauth,
            Some("saved auth"),
        )
        .await
        .unwrap();
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
    let state = TestAppStateBuilder::new().build();
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
    assert!(state.stores.vault.list().await.unwrap().is_empty());
}

#[test]
fn server_secrets_resolve_bootstrap_process_env_before_server_env() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("server.env"),
        "SESSION_SECRET=file-value\nFABRO_DEV_TOKEN=file-dev-token\n",
    )
    .unwrap();

    let secrets = ServerSecrets::load(
        dir.path().join("server.env"),
        HashMap::from([("SESSION_SECRET".to_string(), "env-value".to_string())]),
    )
    .unwrap();

    assert_eq!(secrets.get("SESSION_SECRET").as_deref(), Some("env-value"));
    assert_eq!(
        secrets.get("FABRO_DEV_TOKEN").as_deref(),
        Some("file-dev-token")
    );
}

fn slack_app_state_with_secret_sources(
    vault_entries: &[(&str, &str, SecretType)],
    server_secret_env: HashMap<String, String>,
) -> Arc<AppState> {
    slack_app_state_with_settings_and_secret_sources(
        default_test_server_settings(),
        vault_entries,
        server_secret_env,
    )
}

fn slack_app_state_with_settings_and_secret_sources(
    settings: ServerSettings,
    vault_entries: &[(&str, &str, SecretType)],
    server_secret_env: HashMap<String, String>,
) -> Arc<AppState> {
    let (store, artifact_store) = test_store_bundle();
    let vault_path = test_secret_store_path();
    let server_env_path = vault_path.with_file_name("server.env");
    let mut vault = Vault::load(vault_path.clone()).unwrap();
    for (name, value, secret_type) in vault_entries {
        vault.set(name, value, *secret_type, None).unwrap();
    }
    build_app_state(AppStateConfig {
        subprocess_executable: None,
        resolved_settings: resolved_runtime_settings_for_tests(
            settings,
            RunLayer::default(),
            LlmLayer::default(),
        ),
        execute_in_process: false,
        max_concurrent_runs: 5,
        store,
        artifact_store,
        db_pool: test_db_pool_for_vault_path(&vault_path).expect("test db pool should build"),
        preloaded_vault: vault,
        server_secrets: load_test_server_secrets(server_env_path, server_secret_env),
        env_lookup: default_env_lookup(),
        github_api_base_url: None,
        active_config_path: tempfile::tempdir().unwrap().path().join("settings.toml"),
        http_client: Some(fabro_http::test_http_client().expect("test HTTP client should build")),
        sandbox_inventory: None,
        shutdown: tokio_util::sync::CancellationToken::new(),
        worker_control_bus: None,
        worker_runtime: None,
        automation_materializer_override: None,
    })
    .expect("slack test app state should build")
}

fn slack_test_vault_tokens() -> [(&'static str, &'static str, SecretType); 2] {
    [
        (
            EnvVars::FABRO_SLACK_BOT_TOKEN,
            "xoxb-test",
            SecretType::Token,
        ),
        (
            EnvVars::FABRO_SLACK_APP_TOKEN,
            "xapp-test",
            SecretType::Token,
        ),
    ]
}

#[test]
fn slack_service_ignores_vault_tokens_when_config_is_absent() {
    let state = slack_app_state_with_secret_sources(&slack_test_vault_tokens(), HashMap::new());

    assert!(state.slack_service.is_none());
}

#[test]
fn slack_service_is_enabled_by_config_and_vault_tokens() {
    let state = slack_app_state_with_settings_and_secret_sources(
        server_settings_from_toml(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.integrations.slack]
enabled = true
"#,
        ),
        &[
            (
                EnvVars::FABRO_SLACK_BOT_TOKEN,
                "xoxb-test",
                SecretType::Token,
            ),
            (
                EnvVars::FABRO_SLACK_APP_TOKEN,
                "xapp-test",
                SecretType::Token,
            ),
        ],
        HashMap::new(),
    );

    let service = state
        .slack_service
        .as_ref()
        .expect("slack service should be enabled by config and vault tokens");
    let connection = service.connection_status();
    assert_eq!(connection.kind, IntegrationConnectionKind::SocketMode);
    assert_eq!(connection.status, IntegrationConnectionState::Connecting);
    assert!(connection.last_connected_at.is_none());
    assert!(connection.last_error.is_none());
    assert!(service.default_channel.is_none());
}

#[test]
fn slack_service_receives_configured_default_channel_verbatim() {
    let state = slack_app_state_with_settings_and_secret_sources(
        server_settings_from_toml(
            r##"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.integrations.slack]
enabled = true
default_channel = "#releases"
"##,
        ),
        &slack_test_vault_tokens(),
        HashMap::new(),
    );

    let service = state
        .slack_service
        .as_ref()
        .expect("slack service should be enabled by config and vault tokens");
    assert_eq!(service.default_channel.as_deref(), Some("#releases"));
}

#[test]
fn slack_service_ignores_server_env_tokens() {
    let state = slack_app_state_with_secret_sources(
        &[],
        HashMap::from([
            (
                EnvVars::FABRO_SLACK_BOT_TOKEN.to_string(),
                "xoxb-server-env".to_string(),
            ),
            (
                EnvVars::FABRO_SLACK_APP_TOKEN.to_string(),
                "xapp-server-env".to_string(),
            ),
        ]),
    );

    assert!(state.slack_service.is_none());
}

#[cfg(unix)]
#[test]
fn worker_command_uses_null_stdin_and_token_env() {
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

    assert_worker_command_passes_token_only_by_env(&cmd);
}

#[cfg(unix)]
#[test]
fn worker_command_sets_worker_args() {
    let storage_dir = tempfile::tempdir().unwrap();
    let run_dir = storage_dir.path().join("run-scratch");
    let state = worker_command_test_state(storage_dir.path(), &["dev-token"], Some(TEST_DEV_TOKEN));
    let run_id = RunId::new();

    let cmd = worker_command(
        state.as_ref(),
        run_id,
        RunExecutionMode::Resume,
        &run_dir,
        false,
    )
    .unwrap();

    let args = cmd
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(args, vec![
        "__run-worker".to_string(),
        "--server".to_string(),
        "http://127.0.0.1:32276".to_string(),
        "--storage-dir".to_string(),
        storage_dir.path().display().to_string(),
        "--run-dir".to_string(),
        run_dir.display().to_string(),
        "--run-id".to_string(),
        run_id.to_string(),
        "--mode".to_string(),
        "resume".to_string(),
        "--fabro-home".to_string(),
        fabro_config::Home::from_env().root().display().to_string(),
    ]);
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
fn worker_command_forwards_github_app_private_key_from_vault() {
    let storage_dir = tempfile::tempdir().unwrap();
    let state = worker_command_test_state(storage_dir.path(), &["dev-token"], Some(TEST_DEV_TOKEN));
    let spec = worker_launch_spec(
        state.as_ref(),
        RunId::new(),
        RunExecutionMode::Start,
        storage_dir.path(),
        false,
        Some("test-private-key".to_string()),
        None,
    )
    .unwrap();
    let cmd = LocalWorkerRuntime::command_for_spec(&spec);

    assert_eq!(
        command_env_value(&cmd, EnvVars::GITHUB_APP_PRIVATE_KEY),
        EnvOverride::Set("test-private-key".to_string())
    );
    assert_eq!(
        command_env_value(&cmd, EnvVars::DAYTONA_API_KEY),
        EnvOverride::Unchanged
    );
}

/// A Daytona run's worker carries the vault's key for Petri's Daytona
/// plugin.
#[cfg(unix)]
#[test]
fn worker_command_forwards_daytona_api_key_from_vault() {
    let storage_dir = tempfile::tempdir().unwrap();
    let state = worker_command_test_state(storage_dir.path(), &["dev-token"], Some(TEST_DEV_TOKEN));
    let spec = worker_launch_spec(
        state.as_ref(),
        RunId::new(),
        RunExecutionMode::Start,
        storage_dir.path(),
        false,
        None,
        Some("dtn_test-key".to_string()),
    )
    .unwrap();
    let cmd = LocalWorkerRuntime::command_for_spec(&spec);

    assert_eq!(
        command_env_value(&cmd, EnvVars::DAYTONA_API_KEY),
        EnvOverride::Set("dtn_test-key".to_string())
    );
}

/// A plugin configured under `[server.sandbox.providers.<kind>]` reaches
/// the worker under the names Petri reads, so a run on that kind finds its
/// plugin without a second configuration.
#[cfg(unix)]
#[test]
fn worker_command_forwards_configured_sandbox_plugins() {
    let storage_dir = tempfile::tempdir().unwrap();
    let state = worker_command_test_state_with_extra_config(
        storage_dir.path(),
        &["dev-token"],
        Some(TEST_DEV_TOKEN),
        r#"
[server.sandbox.providers.e2b]
path = "/opt/fabro/plugins/sandbox-driver-e2b"
sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
dev = true
"#,
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
        command_env_value(&cmd, "PETRI_SANDBOX_E2B_PLUGIN"),
        EnvOverride::Set("/opt/fabro/plugins/sandbox-driver-e2b".to_string())
    );
    assert_eq!(
        command_env_value(&cmd, "PETRI_SANDBOX_E2B_SHA256"),
        EnvOverride::Set(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string()
        )
    );
    assert_eq!(
        command_env_value(&cmd, EnvVars::PETRI_SANDBOX_PLUGIN_DEV),
        EnvOverride::Set("1".to_string())
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

fn test_worker_ref(pid: u32) -> WorkerRef {
    WorkerRef::Local { pid }
}

#[cfg(unix)]
fn worker_command(
    state: &AppState,
    run_id: RunId,
    mode: RunExecutionMode,
    run_dir: &Path,
    agent_fabro_tools_enabled: bool,
) -> anyhow::Result<Command> {
    let spec = worker_launch_spec(
        state,
        run_id,
        mode,
        run_dir,
        agent_fabro_tools_enabled,
        None,
        None,
    )?;
    Ok(LocalWorkerRuntime::command_for_spec(&spec))
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
    write_test_server_record(storage_dir);

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

fn write_test_server_record(storage_dir: &Path) {
    let runtime_directory = Storage::new(storage_dir).runtime_directory();
    ServerDaemon::new(
        std::process::id(),
        Bind::Tcp(
            "127.0.0.1:32276"
                .parse::<std::net::SocketAddr>()
                .expect("test bind should parse"),
        ),
        runtime_directory.log_path(),
    )
    .write(&runtime_directory)
    .expect("test server record should be written");
}

/// Waits up to one second for `condition` to hold, re-checking whenever
/// `notify` fires.
async fn wait_until(notify: &Notify, condition: impl Fn() -> bool, expectation: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let notified = notify.notified();
            if condition() {
                return;
            }
            notified.await;
        }
    })
    .await
    .expect(expectation);
}

#[derive(Default)]
struct RecordingWorkerRuntime {
    requested:     StdMutex<Vec<WorkerRef>>,
    forced:        StdMutex<Vec<WorkerRef>>,
    alive:         AtomicBool,
    forced_notify: Notify,
}

impl RecordingWorkerRuntime {
    fn requested_refs(&self) -> Vec<WorkerRef> {
        self.requested
            .lock()
            .expect("requested lock poisoned")
            .clone()
    }

    fn forced_refs(&self) -> Vec<WorkerRef> {
        self.forced.lock().expect("forced lock poisoned").clone()
    }

    fn set_alive(&self, alive: bool) {
        self.alive.store(alive, Ordering::Relaxed);
    }

    async fn wait_for_forced_ref(&self, worker_ref: &WorkerRef) {
        wait_until(
            &self.forced_notify,
            || self.forced_refs().contains(worker_ref),
            "worker should be force-stopped after the cancellation grace period",
        )
        .await;
    }
}

#[async_trait::async_trait]
impl WorkerRuntime for RecordingWorkerRuntime {
    async fn start(&self, _spec: WorkerLaunchSpec) -> anyhow::Result<StartedWorker> {
        anyhow::bail!("recording runtime does not start workers")
    }

    async fn request_stop(&self, worker_ref: &WorkerRef) {
        self.requested
            .lock()
            .expect("requested lock poisoned")
            .push(worker_ref.clone());
    }

    async fn force_stop(&self, worker_ref: &WorkerRef) {
        self.forced
            .lock()
            .expect("forced lock poisoned")
            .push(worker_ref.clone());
        self.alive.store(false, Ordering::Relaxed);
        self.forced_notify.notify_one();
    }

    async fn is_alive(&self, _worker_ref: &WorkerRef) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}

/// How long a test transport waits for a worker's answer: short, so a
/// test whose worker never answers sees `pending` at once.
const TEST_WORKER_CONTROL_ACK_WAIT: Duration = Duration::from_millis(100);

async fn worker_transport_with_receiver(
    run_id: RunId,
) -> (RunAnswerTransport, WorkerControlReceiver) {
    let (transport, receiver, _) = worker_transport_with_acks(run_id).await;
    (transport, receiver)
}

/// A worker transport over a private bus, with the acknowledgements a
/// test answers through.
async fn worker_transport_with_acks(
    run_id: RunId,
) -> (
    RunAnswerTransport,
    WorkerControlReceiver,
    StdArc<WorkerControlAcks>,
) {
    let bus = StdArc::new(LocalWorkerControlBus::new());
    let receiver = bus
        .subscribe(run_id, WorkerControlCursor::Start)
        .await
        .expect("test worker bus should subscribe");
    // Ensure the subscription task is waiting before the test publishes.
    tokio::task::yield_now().await;
    let bus: StdArc<dyn WorkerControlBus> = bus;
    let acks = StdArc::new(WorkerControlAcks::new(TEST_WORKER_CONTROL_ACK_WAIT));
    let transport = RunAnswerTransport::Worker {
        run_id,
        bus,
        acks: StdArc::clone(&acks),
    };
    (transport, receiver, acks)
}

/// A worker that answers every control carrying a request id with
/// `outcome`, as the real worker answers over its control stream. The
/// deliveries it read, for the test to inspect.
fn answering_worker(
    run_id: RunId,
    mut receiver: WorkerControlReceiver,
    acks: StdArc<WorkerControlAcks>,
    outcome: WorkerControlOutcome,
) -> tokio::sync::mpsc::UnboundedReceiver<WorkerControlEnvelope> {
    let (seen_tx, seen_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(Ok(delivery)) = receiver.recv().await {
            if let Some(request_id) = delivery.envelope.request_id() {
                acks.resolve(run_id, WorkerControlAck::new(request_id, outcome.clone()));
            }
            if seen_tx.send(delivery.envelope).is_err() {
                return;
            }
        }
    });
    seen_rx
}

/// The published envelope without the request id the transport added, so
/// a test can compare it with the constructor's.
fn without_request_id(mut envelope: WorkerControlEnvelope) -> WorkerControlEnvelope {
    match &mut envelope.message {
        WorkerControlMessage::Steer { request_id, .. }
        | WorkerControlMessage::Interrupt { request_id, .. }
        | WorkerControlMessage::InterruptThenSteer { request_id, .. } => {
            *request_id = None;
        }
        _ => {}
    }
    envelope
}

async fn recv_worker_control_envelope(
    receiver: &mut WorkerControlReceiver,
) -> WorkerControlEnvelope {
    receiver
        .recv()
        .await
        .expect("test worker control receiver should stay open")
        .expect("test worker control delivery should succeed")
        .envelope
}

#[tokio::test]
async fn worker_answer_transport_cancel_run_publishes_cancel_message() {
    let (transport, mut control_rx) = worker_transport_with_receiver(fixtures::RUN_1).await;

    transport.cancel_run().await.unwrap();

    assert_eq!(
        recv_worker_control_envelope(&mut control_rx).await,
        WorkerControlEnvelope::cancel_run()
    );
}

#[tokio::test]
async fn worker_answer_transport_steer_publishes_plain_steer_message() {
    let (transport, mut control_rx) = worker_transport_with_receiver(fixtures::RUN_1).await;
    let actor = Principal::System {
        system_kind: SystemActorKind::Engine,
    };

    // Nobody answers: the steer is pending once the wait runs out.
    let answer = transport
        .steer("try again".to_string(), None, actor.clone())
        .await
        .unwrap();
    assert_eq!(answer, RunControlAnswer::Pending);

    let envelope = recv_worker_control_envelope(&mut control_rx).await;
    assert!(envelope.request_id().is_some(), "{envelope:?}");
    assert_eq!(
        without_request_id(envelope),
        WorkerControlEnvelope::steer("try again", None, actor)
    );
}

#[tokio::test]
async fn worker_answer_transport_steer_returns_the_workers_answer() {
    let run_id = fixtures::RUN_1;
    let (transport, control_rx, acks) = worker_transport_with_acks(run_id).await;
    let actor = Principal::System {
        system_kind: SystemActorKind::Engine,
    };
    let mut seen = answering_worker(run_id, control_rx, acks, WorkerControlOutcome::Delivered {
        stage: Some("work@1".to_string()),
    });

    let answer = transport
        .steer("try again".to_string(), None, actor)
        .await
        .unwrap();
    assert_eq!(answer, RunControlAnswer::Delivered {
        stage: Some("work@1".to_string()),
    });
    let envelope = seen.recv().await.expect("the worker read the steer");
    assert!(matches!(
        envelope.message,
        WorkerControlMessage::Steer { ref text, .. } if text == "try again"
    ));
}

#[tokio::test]
async fn in_process_transport_answers_a_steer_and_an_interrupt_in_place() {
    let transport = RunAnswerTransport::InProcess {
        interviewer: StdArc::new(ControlInterviewer::new()),
        controls:    RunControls::new(),
    };
    let actor = Principal::System {
        system_kind: SystemActorKind::Engine,
    };

    // The run has no live agent stage: refused at once, no worker asked.
    let answer = transport
        .steer("try again".to_string(), None, actor.clone())
        .await
        .unwrap();
    assert_eq!(answer, RunControlAnswer::Refused {
        code:    "steer_refused".to_string(),
        message: "Steer refused: Run has no active steerable agent session.".to_string(),
    });
    let answer = transport
        .interrupt(Some("work".to_string()), None, actor)
        .await
        .unwrap();
    assert_eq!(answer, RunControlAnswer::Refused {
        code:    "no_such_stage".to_string(),
        message: "Interrupt refused: no stage named `work` is running".to_string(),
    });
}

#[tokio::test]
async fn worker_answer_transport_pause_and_unpause_publish_control_messages() {
    let (transport, mut control_rx) = worker_transport_with_receiver(fixtures::RUN_1).await;

    transport.pause_run().await.unwrap();
    transport.unpause_run().await.unwrap();

    assert_eq!(
        recv_worker_control_envelope(&mut control_rx).await,
        WorkerControlEnvelope::pause_run()
    );
    assert_eq!(
        recv_worker_control_envelope(&mut control_rx).await,
        WorkerControlEnvelope::unpause_run()
    );
}

fn manifest_json(target_path: &str, dot_source: &str) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "cwd": "/tmp",
        "target": {
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

async fn test_intent_with_bearer(
    app: &Router,
    entrypoint: &str,
    source: &str,
    config: Option<&str>,
    bearer: Option<&str>,
) -> serde_json::Value {
    let entrypoint = fabro_types::WorkflowPath::new(entrypoint).unwrap();
    let mut files = std::collections::BTreeMap::from([(entrypoint.clone(), source.to_string())]);
    if let Some(config) = config {
        files.insert(
            entrypoint.resolve_reference("workflow.toml").unwrap(),
            config.to_string(),
        );
    }
    let version =
        fabro_types::WorkflowVersion::new(entrypoint, files, std::collections::BTreeMap::new())
            .unwrap();
    let id = crate::test_support::test_register_workflow_version(app, &version, bearer).await;
    json!({"workflow_version_id": id, "target": {"kind": "none"}, "args": {}})
}

async fn test_intent(app: &Router, source: &str) -> serde_json::Value {
    test_intent_with_bearer(app, "workflow.fabro", source, None, None).await
}

async fn intent_body(app: &Router, source: &str) -> Body {
    Body::from(test_intent(app, source).await.to_string())
}

async fn create_run(app: &Router, dot_source: &str) -> String {
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(intent_body(app, dot_source).await)
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
                .body(intent_body(&app, MINIMAL_DOT).await)
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
                .body(intent_body(&app, MINIMAL_DOT).await)
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
        .stores
        .vault
        .set("OPENAI_API_KEY", "openai-key", SecretType::Token, None)
        .await
        .unwrap();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let body = post_run_intent(&app, test_intent(&app, MINIMAL_DOT).await).await;
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
        .stores
        .vault
        .set("OPENAI_API_KEY", "openai-key", SecretType::Token, None)
        .await
        .unwrap();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let mut intent = test_intent(&app, MINIMAL_DOT).await;
    intent["title"] = json!("Caller title");

    let body = post_run_intent(&app, intent).await;
    let run_id: RunId = body["id"].as_str().unwrap().parse().unwrap();
    // The spawn gate is synchronous in `create_run`, so once the response
    // returns we know no title task was scheduled. No sleep needed.

    assert_eq!(
        state
            .stores
            .run_summaries
            .get(&run_id, Utc::now())
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
async fn create_run_without_ready_llm_provider_rejects_implicit_model_selection() {
    const AGENT_DOT: &str = r#"digraph Test {
    graph [goal="Test"]
    start [shape=Mdiamond]
    work [shape=box, prompt="Do the work"]
    exit  [shape=Msquare]
    start -> work -> exit
}"#;
    let state = TestAppStateBuilder::new().env_lookup(|_| None).build();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    // A workflow with a node that runs a model is refused: no provider is
    // ready, so no model could be chosen for it.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/runs"))
                .header("content-type", "application/json")
                .body(Body::from(test_intent(&app, AGENT_DOT).await.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::UNPROCESSABLE_ENTITY).await;

    assert!(
        body["errors"][0]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("no default model is available")),
        "unexpected response: {body}"
    );
    assert!(state.runs.lock().expect("runs lock poisoned").is_empty());

    // A workflow without one needs no model, so it is admitted.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/runs"))
                .header("content-type", "application/json")
                .body(Body::from(test_intent(&app, MINIMAL_DOT).await.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    response_json!(response, StatusCode::CREATED).await;
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
        .stores
        .vault
        .set("OPENAI_API_KEY", "openai-key", SecretType::Token, None)
        .await
        .unwrap();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let body = post_run_intent(&app, test_intent(&app, MINIMAL_DOT).await).await;
    let run_id: RunId = body["id"].as_str().unwrap().parse().unwrap();
    wait_for_mock_hits(&title_mock, 1).await;
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;

    assert_eq!(
        state
            .stores
            .run_summaries
            .get(&run_id, Utc::now())
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
        .stores
        .vault
        .set("OPENAI_API_KEY", "openai-key", SecretType::Token, None)
        .await
        .unwrap();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let body = post_run_intent(&app, test_intent(&app, MINIMAL_DOT).await).await;
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
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    assert_eq!(
        state
            .stores
            .run_summaries
            .get(&run_id, Utc::now())
            .await
            .unwrap()
            .unwrap()
            .title,
        "User title"
    );
    assert_eq!(title_update_event_count(&state, run_id).await, 1);
}

async fn post_run_intent(app: &Router, intent: serde_json::Value) -> serde_json::Value {
    let response = post_run_intent_response(app, intent).await;
    response_json!(response, StatusCode::CREATED).await
}

async fn post_run_intent_response(app: &Router, intent: serde_json::Value) -> Response {
    app.clone()
        .oneshot(json_request(Method::POST, "/runs", &intent))
        .await
        .unwrap()
}

/// App state whose default environment runs in place on the server, which is
/// the only placement folder targets admit.
fn local_test_app_state() -> Arc<AppState> {
    TestAppStateBuilder::new()
        .default_environment_provider(Some(SandboxProviderKind::LOCAL))
        .vault_entries([(fabro_static::EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
        .build()
}

fn folder_intent(
    workflow_version_id: fabro_types::WorkflowVersionId,
    path: impl serde::Serialize,
) -> serde_json::Value {
    json!({
        "workflow_version_id": workflow_version_id,
        "target": { "kind": "folder", "path": path },
        "args": {}
    })
}

async fn store_workflow_version(
    state: &AppState,
    graph: &str,
    workflow_toml: Option<&str>,
) -> fabro_types::WorkflowVersionId {
    store_workflow_version_with_entrypoint(state, "workflow.fabro", graph, workflow_toml).await
}

async fn store_workflow_version_with_entrypoint(
    state: &AppState,
    entrypoint: &str,
    graph: &str,
    workflow_toml: Option<&str>,
) -> fabro_types::WorkflowVersionId {
    let entrypoint = fabro_types::WorkflowPath::new(entrypoint).unwrap();
    let mut files = std::collections::BTreeMap::from([(entrypoint.clone(), graph.to_string())]);
    if let Some(workflow_toml) = workflow_toml {
        files.insert(
            fabro_types::WorkflowPath::new("workflow.toml").unwrap(),
            workflow_toml.to_string(),
        );
        files.insert(
            fabro_types::WorkflowPath::new("goal.md").unwrap(),
            "Goal loaded from immutable version bytes".to_string(),
        );
        files.insert(
            fabro_types::WorkflowPath::new("Dockerfile").unwrap(),
            "FROM alpine:3".to_string(),
        );
    }
    let version =
        fabro_types::WorkflowVersion::new(entrypoint, files, std::collections::BTreeMap::new())
            .unwrap();
    let version = fabro_workflow_version::ValidatedWorkflowVersion::new(version).unwrap();
    let blobs = state.store_ref().blobs();
    fabro_workflow_version::WorkflowVersionStore::new(blobs)
        .put(&version)
        .await
        .unwrap()
}

#[tokio::test]
async fn post_runs_run_intent_derives_workflow_slug_from_immutable_entrypoint() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    for (entrypoint, expected_slug) in [
        ("deploy/workflow.fabro", "deploy"),
        ("workflow.fabro", "workflow"),
    ] {
        let workflow_version_id =
            store_workflow_version_with_entrypoint(&state, entrypoint, MINIMAL_DOT, None).await;
        let body = post_run_intent(
            &app,
            json!({
                "workflow_version_id": workflow_version_id,
                "target": { "kind": "none" },
                "args": {}
            }),
        )
        .await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();
        let projection = state.load_run_projection(&run_id).await.unwrap();

        assert_eq!(
            projection.spec.workflow_slug.as_deref(),
            Some(expected_slug)
        );
        assert_eq!(
            projection.spec.workflow_version_id,
            Some(workflow_version_id)
        );
    }
}

#[tokio::test]
async fn post_runs_run_intent_persists_tagged_exact_git_target_without_starting() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let workflow_version_id = store_workflow_version(
        &state,
        MINIMAL_DOT,
        Some(
            r#"
_version = 1

[environments.default]
provider = "local"

[environments.default.resources]
cpu = 7

[environments.default.env]
WORKFLOW_OVERLAY = "present"

[run.goal]
file = "goal.md"
"#,
        ),
    )
    .await;
    let submitted_sha = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
    let body = post_run_intent(
        &app,
        json!({
            "workflow_version_id": workflow_version_id,
            "target": {
                "kind": "git",
                "repo": "fabro-sh/fabro",
                "branch": "feature/run-intent",
                "tag": "v1.2.3",
                "sha": submitted_sha
            },
            "args": {
                "inputs": { "ship": true },
                "labels": { "team": "platform" }
            },
            "title": "Intent run"
        }),
    )
    .await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    assert_eq!(body["lifecycle"]["status"]["kind"], "submitted");
    assert_created_then_submitted(&platform_records(&state, run_id).await);
    let projection = state.load_run_projection(&run_id).await.unwrap();
    assert_eq!(
        projection.spec.workflow_version_id,
        Some(workflow_version_id)
    );
    assert_eq!(
        projection.spec.graph.goal(),
        "Goal loaded from immutable version bytes"
    );
    assert_eq!(
        projection.spec.target,
        Some(fabro_types::RunTarget::Git(fabro_types::GitRunTarget {
            repo:   "fabro-sh/fabro".to_string(),
            branch: "feature/run-intent".to_string(),
            tag:    Some("v1.2.3".to_string()),
            sha:    Some("abcdef0123456789abcdef0123456789abcdef01".to_string()),
        }))
    );
    assert_eq!(
        projection
            .spec
            .git
            .as_ref()
            .and_then(|git| git.sha.as_deref()),
        Some("abcdef0123456789abcdef0123456789abcdef01")
    );
    assert_eq!(
        projection.spec.settings.run.inputs["ship"],
        toml::Value::Boolean(true)
    );
    assert_eq!(
        projection.spec.labels.get("team").map(String::as_str),
        Some("platform")
    );
    assert_eq!(
        projection.spec.settings.run.environment.provider,
        SandboxProviderKind::DOCKER
    );
    assert_eq!(
        projection
            .spec
            .settings
            .run
            .environment
            .image
            .docker
            .as_deref(),
        Some("buildpack-deps:noble")
    );
    assert!(
        projection
            .spec
            .settings
            .run
            .environment
            .image
            .dockerfile
            .is_none()
    );
    assert_eq!(
        projection.spec.settings.run.environment.resources.cpu,
        Some(7)
    );
    assert!(
        projection
            .spec
            .settings
            .run
            .environment
            .env
            .contains_key("WORKFLOW_OVERLAY")
    );
}

#[tokio::test]
async fn post_runs_run_intent_creates_submitted_none_target_without_git_projection() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let workflow_version_id = store_workflow_version(&state, MINIMAL_DOT, None).await;
    let body = post_run_intent(
        &app,
        json!({
            "workflow_version_id": workflow_version_id,
            "target": { "kind": "none" },
            "args": {}
        }),
    )
    .await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    assert_eq!(body["lifecycle"]["status"]["kind"], "submitted");
    assert_created_then_submitted(&platform_records(&state, run_id).await);
    let projection = state.load_run_projection(&run_id).await.unwrap();
    assert_eq!(
        projection.spec.target,
        Some(fabro_types::RunTarget::None {})
    );
    assert_eq!(
        projection.spec.workflow_version_id,
        Some(workflow_version_id)
    );
    assert_eq!(projection.spec.source_directory, None);
    assert_eq!(projection.spec.git, None);
    assert!(projection.spec.settings.run.clone.enabled);
    assert!(projection.spec.definition_blob.is_some());
}

#[tokio::test]
async fn post_runs_run_intent_args_true_override_resolved_settings_without_starting() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let state = TestAppStateBuilder::new()
        .default_environment_provider(Some(SandboxProviderKind::LOCAL))
        .env_lookup(|_| None)
        .vault_entries([(EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
        .build();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let workflow_version_id = store_workflow_version(&state, MINIMAL_DOT, None).await;
    let body = post_run_intent(
        &app,
        json!({
            "workflow_version_id": workflow_version_id,
            "target": { "kind": "folder", "path": &workspace },
            "args": {
                "dry_run": true,
                "auto_approve": true,
                "preserve_sandbox": true
            }
        }),
    )
    .await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    assert_eq!(body["lifecycle"]["status"]["kind"], "submitted");
    let projection = state.load_run_projection(&run_id).await.unwrap();
    assert_eq!(projection.spec.settings.run.execution.mode, RunMode::DryRun);
    assert_eq!(
        projection.spec.settings.run.execution.approval,
        ApprovalMode::Auto
    );
    assert!(projection.spec.settings.run.environment.lifecycle.preserve);
}

#[tokio::test]
async fn post_runs_run_intent_dry_run_uses_configured_target_provider() {
    let folder = tempfile::tempdir().unwrap();
    let folder_path = folder
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let cases = [
        (
            test_app_state(),
            None,
            json!({
                "kind": "git",
                "repo": "fabro-sh/fabro",
                "branch": "main"
            }),
            json!({ "dry_run": true }),
        ),
        (
            test_app_state(),
            None,
            json!({ "kind": "none" }),
            json!({ "dry_run": true }),
        ),
        (
            TestAppStateBuilder::new()
                .default_environment_provider(Some(SandboxProviderKind::DAYTONA))
                .vault_entries([(fabro_static::EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
                .build(),
            Some("_version = 1\n[run.execution]\nmode = \"dry_run\"\n"),
            json!({ "kind": "none" }),
            json!({}),
        ),
        (
            TestAppStateBuilder::new()
                .default_environment_provider(Some(SandboxProviderKind::DAYTONA))
                .vault_entries([(fabro_static::EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
                .build(),
            Some("_version = 1\n[run.execution]\nmode = \"dry_run\"\n"),
            json!({
                "kind": "git",
                "repo": "fabro-sh/fabro",
                "branch": "main"
            }),
            json!({}),
        ),
        (
            TestAppStateBuilder::new()
                .runtime_settings(
                    default_test_server_settings(),
                    manifest_run_defaults_from_toml("[run.execution]\nmode = \"dry_run\"\n"),
                )
                .default_environment_provider(Some(SandboxProviderKind::LOCAL))
                .vault_entries([(fabro_static::EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
                .build(),
            None,
            json!({ "kind": "folder", "path": folder_path }),
            json!({}),
        ),
    ];

    for (state, workflow_toml, target, args) in cases {
        let app = crate::test_support::build_test_router(Arc::clone(&state));
        let workflow_version_id = store_workflow_version(&state, MINIMAL_DOT, workflow_toml).await;
        let body = post_run_intent(
            &app,
            json!({
                "workflow_version_id": workflow_version_id,
                "target": target,
                "args": args
            }),
        )
        .await;
        let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();
        let projection = state.load_run_projection(&run_id).await.unwrap();

        assert_eq!(projection.spec.settings.run.execution.mode, RunMode::DryRun);
        assert_eq!(
            serde_json::to_value(projection.spec.target.clone().unwrap()).unwrap(),
            target
        );
    }
}

#[tokio::test]
async fn post_runs_run_intent_dry_run_rejects_configured_target_mismatches() {
    let states_and_targets = [
        (
            local_test_app_state(),
            json!({
                "kind": "git",
                "repo": "fabro-sh/fabro",
                "branch": "main"
            }),
        ),
        (local_test_app_state(), json!({ "kind": "none" })),
        (
            test_app_state(),
            json!({ "kind": "folder", "path": "/path-that-must-not-be-read" }),
        ),
        (
            TestAppStateBuilder::new()
                .default_environment_provider(Some(SandboxProviderKind::DAYTONA))
                .vault_entries([(fabro_static::EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
                .build(),
            json!({ "kind": "folder", "path": "/path-that-must-not-be-read" }),
        ),
    ];

    for (state, target) in states_and_targets {
        let app = crate::test_support::build_test_router(Arc::clone(&state));
        let workflow_version_id = store_workflow_version(&state, MINIMAL_DOT, None).await;
        let response = post_run_intent_response(
            &app,
            json!({
                "workflow_version_id": workflow_version_id,
                "target": target,
                "args": { "dry_run": true }
            }),
        )
        .await;
        let body = response_json!(response, StatusCode::UNPROCESSABLE_ENTITY).await;

        assert_eq!(body["errors"][0]["code"], "target_environment_unsupported");
        assert!(
            state
                .stores
                .run_summaries
                .list_identities()
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn post_runs_run_intent_args_false_are_distinct_from_omitted_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let state = TestAppStateBuilder::new()
        .runtime_settings(
            default_test_server_settings(),
            manifest_run_defaults_from_toml(
                r#"
[run.execution]
mode = "dry_run"
approval = "auto"

[run.environment.lifecycle]
preserve = true
"#,
            ),
        )
        .default_environment_provider(Some(SandboxProviderKind::LOCAL))
        .env_lookup(|_| None)
        .vault_entries([(EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
        .build();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let workflow_version_id = store_workflow_version(&state, MINIMAL_DOT, None).await;

    let explicit_false = post_run_intent(
        &app,
        json!({
            "workflow_version_id": workflow_version_id,
            "target": { "kind": "folder", "path": &workspace },
            "args": {
                "dry_run": false,
                "auto_approve": false,
                "preserve_sandbox": false
            }
        }),
    )
    .await;
    let omitted = post_run_intent(
        &app,
        json!({
            "workflow_version_id": workflow_version_id,
            "target": { "kind": "folder", "path": &workspace },
            "args": {}
        }),
    )
    .await;

    assert_eq!(explicit_false["lifecycle"]["status"]["kind"], "submitted");
    assert_eq!(omitted["lifecycle"]["status"]["kind"], "submitted");
    let explicit_false_id = explicit_false["id"]
        .as_str()
        .unwrap()
        .parse::<RunId>()
        .unwrap();
    let omitted_id = omitted["id"].as_str().unwrap().parse::<RunId>().unwrap();
    let explicit_false = state.load_run_projection(&explicit_false_id).await.unwrap();
    let omitted = state.load_run_projection(&omitted_id).await.unwrap();

    assert_eq!(
        explicit_false.spec.settings.run.execution.mode,
        RunMode::Normal
    );
    assert_eq!(
        explicit_false.spec.settings.run.execution.approval,
        ApprovalMode::Prompt
    );
    assert!(
        !explicit_false
            .spec
            .settings
            .run
            .environment
            .lifecycle
            .preserve
    );
    assert_eq!(omitted.spec.settings.run.execution.mode, RunMode::DryRun);
    assert_eq!(
        omitted.spec.settings.run.execution.approval,
        ApprovalMode::Auto
    );
    assert!(omitted.spec.settings.run.environment.lifecycle.preserve);
}

#[tokio::test]
async fn post_runs_run_intent_canonicalizes_and_persists_a_local_folder_target() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let hop = dir.path().join("hop");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&hop).unwrap();
    // Target-project files are not compiler inputs for a version-backed run.
    std::fs::write(workspace.join("workflow.toml"), "not valid TOML").unwrap();
    std::fs::write(workspace.join("goal.md"), "Goal from target folder").unwrap();
    let submitted = hop.join("..").join("workspace");
    let canonical = workspace
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let state = local_test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let workflow_version_id = store_workflow_version(
        &state,
        MINIMAL_DOT,
        Some("_version = 1\n[run.goal]\nfile = \"goal.md\"\n"),
    )
    .await;

    let body = post_run_intent(
        &app,
        folder_intent(workflow_version_id, submitted.to_string_lossy()),
    )
    .await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    assert_eq!(body["lifecycle"]["status"]["kind"], "submitted");
    assert_created_then_submitted(&platform_records(&state, run_id).await);
    let projection = state.load_run_projection(&run_id).await.unwrap();
    assert_eq!(
        projection.spec.target,
        Some(fabro_types::RunTarget::Folder {
            path: canonical.clone(),
        })
    );
    assert_eq!(
        projection.spec.source_directory.as_deref(),
        Some(canonical.as_str())
    );
    assert_eq!(projection.spec.git, None);
    assert_eq!(
        projection.spec.graph.goal(),
        "Goal loaded from immutable version bytes"
    );
    assert_eq!(
        projection.spec.settings.run.environment.provider,
        SandboxProviderKind::LOCAL
    );
    assert!(projection.spec.definition_blob.is_some());
}

#[tokio::test]
async fn post_runs_run_intent_rejects_automatic_pull_requests_for_local_environment() {
    let target = tempfile::tempdir().unwrap();
    let state = local_test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let workflow_version_id = store_workflow_version(
        &state,
        MINIMAL_DOT,
        Some("_version = 1\n[run.pull_request]\nenabled = true\n"),
    )
    .await;

    let response =
        post_run_intent_response(&app, folder_intent(workflow_version_id, target.path())).await;
    let body = response_json!(response, StatusCode::UNPROCESSABLE_ENTITY).await;

    assert_eq!(
        body["errors"][0]["code"],
        "pull_request_environment_unsupported"
    );
    assert_eq!(
        body["errors"][0]["detail"],
        "automatic pull requests require a clone-based Docker or Daytona environment; disable run.pull_request.enabled for Local execution"
    );
    assert!(state.runs.lock().expect("runs lock poisoned").is_empty());
    assert!(
        state
            .stores
            .run_summaries
            .list_identities()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn post_runs_run_intent_accepts_disabled_pull_requests_for_local_environment() {
    let target = tempfile::tempdir().unwrap();
    let state = local_test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let workflow_version_id = store_workflow_version(
        &state,
        MINIMAL_DOT,
        Some("_version = 1\n[run.pull_request]\nenabled = false\n"),
    )
    .await;

    // `post_run_intent` asserts the `201 Created` admission outcome.
    post_run_intent(&app, folder_intent(workflow_version_id, target.path())).await;
}

#[tokio::test]
async fn post_runs_run_intent_accepts_automatic_pull_requests_for_configured_docker_dry_run() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let workflow_version_id = store_workflow_version(
        &state,
        MINIMAL_DOT,
        Some("_version = 1\n[run.pull_request]\nenabled = true\n"),
    )
    .await;

    let body = post_run_intent(
        &app,
        json!({
            "workflow_version_id": workflow_version_id,
            "target": {
                "kind": "git",
                "repo": "fabro-sh/fabro",
                "branch": "main"
            },
            "args": { "dry_run": true }
        }),
    )
    .await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();
    let projection = state.load_run_projection(&run_id).await.unwrap();

    assert_eq!(
        projection.spec.settings.run.environment.provider,
        SandboxProviderKind::DOCKER
    );
    assert_eq!(projection.spec.settings.run.execution.mode, RunMode::DryRun);
    assert!(projection.spec.settings.run.pull_request.is_some());
}

#[tokio::test]
async fn post_runs_run_intent_observes_folder_git_metadata_without_a_remote_call() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let mut index = repo.index().unwrap();
    let tree_id = index.write_tree().unwrap();
    drop(index);
    let tree = repo.find_tree(tree_id).unwrap();
    let signature = git2::Signature::now("Fabro Test", "fabro@example.com").unwrap();
    let commit = repo
        .commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
    let commit = commit.to_string();
    drop(tree);
    repo.remote("origin", "https://github.com/acme/widgets.git")
        .unwrap();
    drop(repo);
    let canonical = dir
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let state = local_test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let workflow_version_id = store_workflow_version(&state, MINIMAL_DOT, None).await;

    let body = post_run_intent(&app, folder_intent(workflow_version_id, canonical)).await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();
    let projection = state.load_run_projection(&run_id).await.unwrap();
    let git = projection.spec.git.clone().unwrap();

    assert_eq!(git.origin_url, "https://github.com/acme/widgets");
    assert!(!git.branch.is_empty());
    assert_eq!(git.sha.as_deref(), Some(commit.as_str()));
    assert_eq!(git.dirty, fabro_types::DirtyStatus::Clean);
}

#[tokio::test]
async fn post_runs_run_intent_rejects_invalid_folder_paths_before_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    std::fs::write(&file, "not a directory").unwrap();
    let state = local_test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let workflow_version_id = store_workflow_version(&state, MINIMAL_DOT, None).await;
    let invalid_paths = [
        String::new(),
        "relative/path".to_string(),
        dir.path().join("missing").to_string_lossy().to_string(),
        file.to_string_lossy().to_string(),
    ];

    for path in invalid_paths {
        let response =
            post_run_intent_response(&app, folder_intent(workflow_version_id, path)).await;
        let body = response_json!(response, StatusCode::UNPROCESSABLE_ENTITY).await;
        assert_eq!(body["errors"][0]["code"], "target_invalid");
    }

    assert!(state.runs.lock().expect("runs lock poisoned").is_empty());
    assert!(
        state
            .stores
            .run_summaries
            .list_identities()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn post_runs_run_intent_applies_the_folder_target_environment_matrix() {
    let dir = tempfile::tempdir().unwrap();
    // A missing path proves provider admission wins over filesystem
    // materialization. Touching the path first would return `target_invalid`
    // instead of the provider-specific errors asserted below.
    let target = dir.path().join("missing").to_string_lossy().to_string();

    for state in [
        test_app_state(),
        TestAppStateBuilder::new()
            .default_environment_provider(Some(SandboxProviderKind::DAYTONA))
            .vault_entries([(fabro_static::EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
            .build(),
    ] {
        let app = crate::test_support::build_test_router(Arc::clone(&state));
        let workflow_version_id = store_workflow_version(&state, MINIMAL_DOT, None).await;
        let response =
            post_run_intent_response(&app, folder_intent(workflow_version_id, &target)).await;
        let body = response_json!(response, StatusCode::UNPROCESSABLE_ENTITY).await;
        assert_eq!(body["errors"][0]["code"], "target_environment_unsupported");
        assert!(
            state
                .stores
                .run_summaries
                .list_identities()
                .await
                .unwrap()
                .is_empty()
        );
    }

    let disabled_state = TestAppStateBuilder::new()
        .runtime_settings(
            server_settings_from_toml(
                r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.sandbox.providers.local]
enabled = false
"#,
            ),
            RunLayer::default(),
        )
        .default_environment_provider(Some(SandboxProviderKind::LOCAL))
        .vault_entries([(fabro_static::EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
        .build();
    let app = crate::test_support::build_test_router(Arc::clone(&disabled_state));
    let workflow_version_id = store_workflow_version(&disabled_state, MINIMAL_DOT, None).await;
    let response =
        post_run_intent_response(&app, folder_intent(workflow_version_id, &target)).await;
    let body = response_json!(response, StatusCode::SERVICE_UNAVAILABLE).await;
    assert_eq!(body["errors"][0]["code"], "integration_unavailable");
    assert!(
        disabled_state
            .stores
            .run_summaries
            .list_identities()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn run_tools_worker_cannot_select_server_folder_from_clone_based_parent() {
    let dir = tempfile::tempdir().unwrap();
    let missing_target = dir.path().join("missing");
    let (state, app) = jwt_auth_app();
    let user_token = issue_test_user_jwt();
    let parent_run_id = create_run_with_bearer(&app, &user_token).await;
    let worker_token = issue_test_run_tools_worker_token(&parent_run_id);
    let workflow_version_id = store_workflow_version(&state, MINIMAL_DOT, None).await;
    let mut intent = folder_intent(workflow_version_id, missing_target.to_string_lossy());
    intent["environment_id"] = json!("local");
    intent["parent_id"] = json!(parent_run_id);

    let response = app
        .oneshot(json_bearer_request(
            Method::POST,
            "/runs",
            &worker_token,
            &intent,
        ))
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::UNPROCESSABLE_ENTITY).await;

    assert_eq!(body["errors"][0]["code"], "target_environment_unsupported");
    assert_eq!(
        body["errors"][0]["detail"],
        "folder targets created by a worker require a Local parent environment"
    );
    assert_eq!(
        state
            .stores
            .run_summaries
            .list_identities()
            .await
            .unwrap()
            .len(),
        1,
        "the rejected child must not be persisted"
    );
}

#[tokio::test]
async fn run_tools_worker_folder_target_from_missing_parent_run_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let (state, app) = jwt_auth_app();
    let worker_token = issue_test_run_tools_worker_token(&RunId::new());
    let workflow_version_id = store_workflow_version(&state, MINIMAL_DOT, None).await;
    let mut intent = folder_intent(workflow_version_id, dir.path().to_string_lossy());
    intent["environment_id"] = json!("local");

    let response = app
        .oneshot(json_bearer_request(
            Method::POST,
            "/runs",
            &worker_token,
            &intent,
        ))
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::NOT_FOUND).await;

    assert_eq!(body["errors"][0]["code"], "worker_run_not_found");
    assert!(
        state
            .stores
            .run_summaries
            .list_identities()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn run_tools_worker_can_select_server_folder_from_local_parent() {
    let dir = tempfile::tempdir().unwrap();
    let (state, app) = jwt_auth_app();
    let user_token = issue_test_user_jwt();
    let workflow_version_id = store_workflow_version(&state, MINIMAL_DOT, None).await;
    let mut parent_intent = folder_intent(workflow_version_id, dir.path().to_string_lossy());
    parent_intent["environment_id"] = json!("local");

    let response = app
        .clone()
        .oneshot(json_bearer_request(
            Method::POST,
            "/runs",
            &user_token,
            &parent_intent,
        ))
        .await
        .unwrap();
    let parent = response_json!(response, StatusCode::CREATED).await;
    let parent_run_id = parent["id"].as_str().unwrap().parse::<RunId>().unwrap();
    let worker_token = issue_test_run_tools_worker_token(&parent_run_id);
    let mut child_intent = folder_intent(workflow_version_id, dir.path().to_string_lossy());
    child_intent["environment_id"] = json!("local");
    child_intent["parent_id"] = json!(parent_run_id);

    let response = app
        .oneshot(json_bearer_request(
            Method::POST,
            "/runs",
            &worker_token,
            &child_intent,
        ))
        .await
        .unwrap();
    let child = response_json!(response, StatusCode::CREATED).await;

    assert_eq!(child["parent_id"], parent_run_id.to_string());
    assert_eq!(child["lifecycle"]["status"]["kind"], "submitted");
}

#[tokio::test]
async fn post_runs_run_intent_accepts_none_target_with_ready_daytona_environment() {
    let state = TestAppStateBuilder::new()
        .default_environment_provider(Some(SandboxProviderKind::DAYTONA))
        .vault_entries([
            (fabro_static::EnvVars::OPENAI_API_KEY, "test-openai-api-key"),
            (
                fabro_static::EnvVars::DAYTONA_API_KEY,
                "test-daytona-api-key",
            ),
        ])
        .build();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let workflow_version_id = store_workflow_version(&state, MINIMAL_DOT, None).await;
    let body = post_run_intent(
        &app,
        json!({
            "workflow_version_id": workflow_version_id,
            "target": { "kind": "none" },
            "args": {}
        }),
    )
    .await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    let projection = state.load_run_projection(&run_id).await.unwrap();
    assert_eq!(
        projection.spec.target,
        Some(fabro_types::RunTarget::None {})
    );
    assert_eq!(
        projection.spec.settings.run.environment.provider,
        SandboxProviderKind::DAYTONA
    );
    assert_eq!(projection.spec.source_directory, None);
    assert_eq!(projection.spec.git, None);
}

#[tokio::test]
async fn post_runs_reports_malformed_json() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let malformed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/runs"))
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await
        .unwrap();
    let malformed = response_json!(malformed, StatusCode::BAD_REQUEST).await;
    assert_eq!(malformed["errors"][0]["code"], "invalid_json");
}

#[tokio::test]
async fn post_runs_attributes_parse_failures_and_rejects_duplicate_keys() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let id = fabro_types::test_support::test_workflow_version_id();
    for (raw, expected_detail) in [
        ("{}".to_string(), "missing field"),
        (
            format!(
                r#"{{"workflow_version_id":"{id}","target":{{"kind":"none"}},"unexpected":true,"args":{{}}}}"#
            ),
            "unknown field",
        ),
        (
            format!(
                r#"{{"workflow_version_id":"{id}","workflow_version_id":"{id}","target":{{"kind":"none"}},"args":{{}}}}"#
            ),
            "duplicate field",
        ),
        (
            format!(
                r#"{{"workflow_version_id":"{id}","target":{{"kind":"none"}},"args":{{"dry_run":true,"dry_run":false}}}}"#
            ),
            "duplicate field",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(api("/runs"))
                    .header("content-type", "application/json")
                    .body(Body::from(raw))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response_json!(response, StatusCode::UNPROCESSABLE_ENTITY).await;
        assert_eq!(body["errors"][0]["code"], "run_intent_invalid");
        assert!(
            body["errors"][0]["detail"]
                .as_str()
                .unwrap()
                .contains(expected_detail)
        );
    }
    assert!(state.runs.lock().unwrap().is_empty());
    assert!(
        state
            .stores
            .run_summaries
            .list_identities()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn post_runs_run_intent_maps_missing_version_environment_and_target_errors() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let missing_version_id = fabro_types::test_support::test_workflow_version_id();
    let base_intent = json!({
        "workflow_version_id": missing_version_id,
        "target": {
            "kind": "git",
            "repo": "fabro-sh/fabro",
            "branch": "feature/run-intent"
        },
        "args": {}
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/runs"))
                .header("content-type", "application/json")
                .body(Body::from(base_intent.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::NOT_FOUND).await;
    assert_eq!(body["errors"][0]["code"], "workflow_version_not_found");

    let workflow_version_id = store_workflow_version(&state, MINIMAL_DOT, None).await;
    for (patch, expected_status, expected_code) in [
        (
            json!({ "environment_id": "missing-environment" }),
            StatusCode::NOT_FOUND,
            "environment_not_found",
        ),
        (
            json!({ "environment_id": "not valid" }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "run_intent_invalid",
        ),
        (
            json!({ "target": { "kind": "git", "repo": "fabro-sh/fabro", "branch": "heads/main" } }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "target_invalid",
        ),
    ] {
        let mut intent = json!({
            "workflow_version_id": workflow_version_id,
            "target": {
                "kind": "git",
                "repo": "fabro-sh/fabro",
                "branch": "feature/run-intent"
            },
            "args": {}
        });
        for (key, value) in patch.as_object().unwrap() {
            intent[key] = value.clone();
        }
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(api("/runs"))
                    .header("content-type", "application/json")
                    .body(Body::from(intent.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response_json!(response, expected_status).await;
        assert_eq!(body["errors"][0]["code"], expected_code);
    }

    assert!(state.runs.lock().expect("runs lock poisoned").is_empty());
}

#[tokio::test]
async fn post_runs_run_intent_rejects_none_target_with_local_environment_before_persistence() {
    let state = local_test_app_state();
    let workflow_version_id = store_workflow_version(&state, MINIMAL_DOT, None).await;
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/runs"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "workflow_version_id": workflow_version_id,
                        "target": { "kind": "none" },
                        "args": {}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = response_json!(response, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(body["errors"][0]["code"], "target_environment_unsupported");
    assert!(state.runs.lock().expect("runs lock poisoned").is_empty());
    assert!(
        state
            .stores
            .run_summaries
            .list_identities()
            .await
            .unwrap()
            .is_empty()
    );
}

/// Posts a Git and a `none` run intent against `state` and asserts both are
/// rejected as `integration_unavailable` without persisting anything.
async fn assert_run_intent_targets_unavailable(state: &Arc<AppState>) {
    let version_id = store_workflow_version(state, MINIMAL_DOT, None).await;
    let app = crate::test_support::build_test_router(Arc::clone(state));
    for target in [
        json!({
            "kind": "git",
            "repo": "fabro-sh/fabro",
            "branch": "feature/run-intent"
        }),
        json!({ "kind": "none" }),
    ] {
        let intent = json!({
            "workflow_version_id": version_id,
            "target": target,
            "args": {}
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(api("/runs"))
                    .header("content-type", "application/json")
                    .body(Body::from(intent.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response_json!(response, StatusCode::SERVICE_UNAVAILABLE).await;
        assert_eq!(body["errors"][0]["code"], "integration_unavailable");
    }
    assert!(state.runs.lock().expect("runs lock poisoned").is_empty());
    assert!(
        state
            .stores
            .run_summaries
            .list_identities()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn post_runs_run_intent_rejects_disabled_or_unready_sandbox_integrations() {
    let disabled_state = test_app_state_with_options(
        server_settings_from_toml(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.sandbox.providers.docker]
enabled = false
"#,
        ),
        RunLayer::default(),
        5,
    );
    assert_run_intent_targets_unavailable(&disabled_state).await;

    let daytona_state = TestAppStateBuilder::new()
        .default_environment_provider(Some(SandboxProviderKind::DAYTONA))
        .vault_entries([(fabro_static::EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
        .build();
    assert_run_intent_targets_unavailable(&daytona_state).await;
}

#[tokio::test]
async fn create_run_from_intent_helper_persists_automation_version_and_exact_target() {
    let state = TestAppStateBuilder::new()
        .env_lookup(|_| None)
        .vault_entries([(EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
        .build();
    let workflow_version_id = store_workflow_version(&state, MINIMAL_DOT, None).await;
    let run_id = RunId::new();
    let automation = fabro_types::AutomationRef {
        id:              "nightly".to_string(),
        name:            Some("Nightly".to_string()),
        trigger_id:      Some("schedule".to_string()),
        workflow_source: None,
    };
    let target = RunTarget::Git(GitRunTarget {
        repo:   "fabro-sh/fabro".to_string(),
        branch: "main".to_string(),
        tag:    Some("v1.2.3".to_string()),
        sha:    Some("0123456789abcdef0123456789abcdef01234567".to_string()),
    });

    let response = Box::pin(handler::runs::create_run_from_intent(
        Arc::clone(&state),
        handler::runs::CreateRunFromIntentRequest {
            intent:          fabro_api::types::RunIntent {
                workflow_version_id,
                target: target.clone(),
                args: fabro_api::types::RunIntentArgs::default(),
                environment_id: None,
                parent_id: None,
                title: None,
                goal: None,
            },
            explicit_run_id: Some(run_id),
            actor:           Principal::System {
                system_kind: SystemActorKind::Engine,
            },
            headers:         HeaderMap::new(),
            automation:      Some(automation.clone()),
        },
    ))
    .await;

    let body = response_json!(response, StatusCode::CREATED).await;
    assert_eq!(body["automation"]["id"], automation.id);
    let summary = state
        .stores
        .run_summaries
        .get(&run_id, Utc::now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.automation, Some(automation.clone()));
    let projection = state.load_run_projection(&run_id).await.unwrap();
    assert_eq!(
        projection.spec.workflow_version_id,
        Some(workflow_version_id)
    );
    assert_eq!(projection.spec.target, Some(target));
    assert_eq!(projection.spec.automation, Some(automation));
    assert_created_then_submitted(&platform_records(&state, run_id).await);
}

#[tokio::test]
async fn fake_automation_materializer_injection_captures_input_and_returns_version() {
    let fake = TestAutomationRunMaterializer::succeed(GitRunTarget {
        repo:   "fabro-sh/fabro".to_string(),
        branch: "main".to_string(),
        tag:    None,
        sha:    Some("0123456789abcdef0123456789abcdef01234567".to_string()),
    });
    let state = TestAppStateBuilder::new()
        .automation_materializer(fake.clone())
        .build();
    let run_id = RunId::new();
    let temp_root = PathBuf::from("/tmp/fabro/automation");
    let target = GitRunTarget {
        repo:   "fabro-sh/fabro".to_string(),
        branch: "main".to_string(),
        tag:    None,
        sha:    None,
    };

    let output = state
        .materialize_automation_run(AutomationRunMaterializeInput {
            automation_id: AutomationId::new("nightly").unwrap(),
            target: target.clone(),
            workflow_source: None,
            workflow: "demo".to_string(),
            run_id,
            temp_root: temp_root.clone(),
        })
        .await
        .expect("fake materializer should succeed");

    let stored = fabro_workflow_version::WorkflowVersionStore::new(state.store_ref().blobs())
        .get(&output.workflow_version_id)
        .await
        .unwrap();
    assert!(stored.is_some());
    let captured = fake.captured_inputs();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].automation_id.as_str(), "nightly");
    assert_eq!(captured[0].target, target);
    assert_eq!(captured[0].workflow, "demo");
    assert_eq!(captured[0].run_id, run_id);
    assert_eq!(captured[0].temp_root, temp_root);
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
            .stores
            .run_summaries
            .get(&run_id, Utc::now())
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
    platform_records(state, run_id)
        .await
        .iter()
        .filter(|stored| matches!(stored.record, PlatformRecord::RunTitle(_)))
        .count()
}

/// Every platform record of the run, in seq order.
async fn platform_records(state: &AppState, run_id: RunId) -> Vec<StoredPlatformRecord> {
    state
        .stores
        .run_summaries
        .platform_records()
        .read(&run_id)
        .await
        .unwrap()
}

/// The records a freshly created run holds: `run.created`, then the
/// `submitted` lifecycle transition.
fn assert_created_then_submitted(records: &[StoredPlatformRecord]) {
    assert_eq!(
        records
            .iter()
            .map(|stored| stored.record.kind().to_string())
            .collect::<Vec<_>>(),
        ["run.created", "run.lifecycle"]
    );
    let PlatformRecord::RunLifecycle(lifecycle) = &records[1].record else {
        panic!("the second record should be a lifecycle transition");
    };
    assert_eq!(lifecycle.transition, RunLifecycleKind::Submitted);
}

/// The run's lifecycle transitions of `kind`.
fn lifecycle_transition_count(records: &[StoredPlatformRecord], kind: RunLifecycleKind) -> usize {
    records
        .iter()
        .filter(|stored| {
            matches!(
                &stored.record,
                PlatformRecord::RunLifecycle(lifecycle) if lifecycle.transition == kind
            )
        })
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
    let state = TestAppStateBuilder::new()
        .llm_overlay_toml(&acme_overlay("https://api.acme.test/v1"))
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

/// An input a bundled prompt reads that nothing binds is Petri's
/// `unsupported.template.unbound_input`, positioned at the node attribute
/// that names the prompt file, in the workflow the manifest targets.
#[tokio::test]
async fn validate_endpoint_reports_an_unbound_input_with_petris_code_and_position() {
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
        .find(|diagnostic| diagnostic["rule"] == "unsupported.template.unbound_input")
        .unwrap_or_else(|| panic!("expected Petri's unbound input diagnostic: {diagnostics:?}"));

    assert_eq!(diagnostic["severity"], "error");
    assert_eq!(diagnostic["source_path"], "workflow.fabro");
    assert_eq!(diagnostic["line"], 4);
    assert_eq!(diagnostic["column"], 43);
    let message = diagnostic["message"].as_str().unwrap();
    assert!(message.contains("test_imported_prompt"), "{message}");
    assert!(message.contains("inputs.foo"), "{message}");
    assert_eq!(body["ok"], false);
}

async fn create_run_for_target(app: &Router, target_path: &str, dot_source: &str) -> String {
    let intent = test_intent_with_bearer(app, target_path, dot_source, None, None).await;
    post_run_intent(app, intent).await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn create_run_for_target_with_workflow_name(
    app: &Router,
    target_path: &str,
    dot_source: &str,
    workflow_name: &str,
) -> String {
    let config = format!("_version = 1\n[workflow]\nname = {workflow_name:?}\n");
    let intent = test_intent_with_bearer(app, target_path, dot_source, Some(&config), None).await;
    post_run_intent(app, intent).await["id"]
        .as_str()
        .unwrap()
        .to_string()
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

/// Write one artifact for a stage the way the hooks do: straight into the
/// artifact store.
async fn seed_stage_artifact(
    state: &AppState,
    run_id: &str,
    stage_id: &str,
    retry: u32,
    relative_path: &str,
    bytes: &[u8],
) {
    let run_id = run_id.parse::<RunId>().unwrap();
    let key = ArtifactKey::new(stage_id.parse::<StageId>().unwrap(), retry, relative_path);
    let mut writer = state.artifact_store.writer(&run_id, &key).unwrap();
    writer.write_all(bytes).await.unwrap();
    writer.shutdown().await.unwrap();
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

fn test_priced_usage(
    model_id: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> fabro_types::ModelUsage {
    fabro_types::ModelUsage::new(
        ModelRef::new(
            lithos_llm::catalog::builtin::openai(),
            ModelId::new(model_id),
        ),
        Usage {
            tokens: TokenCounts {
                input: input_tokens,
                output: output_tokens,
                ..TokenCounts::default()
            },
            cost:   Some(Cost {
                usd_micros: input_tokens + output_tokens,
                source:     CostSource::Catalog,
            }),
        },
    )
}

/// Seed the lifecycle transitions a worker would have recorded, so the
/// run's projection stands where the test needs it.
async fn seed_lifecycle(state: &AppState, run_id: RunId, records: Vec<RunLifecycleRecord>) {
    for record in records {
        run_records::lifecycle(state, run_id, record).await.unwrap();
    }
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
        LlmLayer::default(),
    )
}

fn create_github_token_app_state_with_env_lookup_and_llm_catalog_settings(
    token: Option<&str>,
    github_api_base_url: Option<String>,
    env_lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    llm_overlay: LlmLayer,
) -> Arc<AppState> {
    let (store, artifact_store) = test_store_bundle();
    let vault_path = test_secret_store_path();
    let server_env_path = vault_path.with_file_name("server.env");
    let active_config_path = vault_path.with_file_name("settings.toml");
    if let Some(token) = token {
        Vault::load(vault_path.clone())
            .expect("test vault should load")
            .set("GITHUB_TOKEN", token, SecretType::Token, None)
            .expect("test github token should be writable");
    }
    Vault::load(vault_path.clone())
        .expect("test vault should load")
        .set(
            EnvVars::OPENAI_API_KEY,
            "test-openai-api-key",
            SecretType::Token,
            None,
        )
        .expect("test OpenAI credential should be writable");
    let db_pool = test_db_pool_for_vault_path(&vault_path).expect("test db pool should build");
    let preloaded_vault = crate::test_support::test_secret_snapshot(db_pool.clone())
        .expect("test secret snapshot should build");
    let config = AppStateConfig {
        subprocess_executable: None,
        resolved_settings: resolved_runtime_settings_for_tests(
            github_token_settings(),
            RunLayer::default(),
            llm_overlay,
        ),
        execute_in_process: false,
        max_concurrent_runs: 5,
        store,
        artifact_store,
        db_pool,
        preloaded_vault,
        server_secrets: load_test_server_secrets(server_env_path, HashMap::new()),
        env_lookup: Arc::new(env_lookup),
        github_api_base_url,
        active_config_path,
        http_client: Some(fabro_http::test_http_client().expect("test HTTP client should build")),
        sandbox_inventory: None,
        shutdown: tokio_util::sync::CancellationToken::new(),
        worker_control_bus: None,
        worker_runtime: None,
        automation_materializer_override: None,
    };
    build_app_state(config).expect("test app state should build")
}

#[tokio::test]
async fn github_token_strategy_ignores_process_env_token() {
    let state = create_github_token_app_state_with_env_lookup(None, None, |name| match name {
        EnvVars::GITHUB_TOKEN => Some("ghu_from_env".to_string()),
        _ => None,
    });
    let settings = state.server_settings();

    let err = state
        .github_credentials(&settings.server.integrations.github)
        .await
        .expect_err("server runtime should ignore env-backed GitHub tokens");

    assert_eq!(
        err.to_string(),
        "GITHUB_TOKEN not configured -- run fabro install or run fabro secret set GITHUB_TOKEN"
    );
}

#[tokio::test]
async fn github_token_strategy_ignores_gh_token_alias() {
    let state = create_github_token_app_state_with_env_lookup(None, None, |name| match name {
        EnvVars::GH_TOKEN => Some("ghu_from_env_alias".to_string()),
        _ => None,
    });
    state
        .stores
        .vault
        .set(
            EnvVars::GH_TOKEN,
            "ghu_from_vault_alias",
            SecretType::Token,
            None,
        )
        .await
        .unwrap();
    let settings = state.server_settings();

    let err = state
        .github_credentials(&settings.server.integrations.github)
        .await
        .expect_err("server runtime should ignore GH_TOKEN in env and vault");

    assert_eq!(
        err.to_string(),
        "GITHUB_TOKEN not configured -- run fabro install or run fabro secret set GITHUB_TOKEN"
    );
}

#[tokio::test]
async fn github_token_strategy_reads_github_token_from_vault() {
    let state = create_github_token_app_state(Some("ghu_test"), None);
    let settings = state.server_settings();

    let credentials = state
        .github_credentials(&settings.server.integrations.github)
        .await
        .expect("vault GitHub token should resolve")
        .expect("vault GitHub token should produce credentials");

    assert!(
        matches!(credentials, fabro_github::GitHubCredentials::Pat(token) if token == "ghu_test")
    );
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
async fn test_model_explicit_provider_alias_returns_canonical_model_id_when_unavailable() {
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        |_| None,
    );
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/models/sonnet/test?provider=anthropic"))
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["model_id"], "claude-sonnet-5");
    assert_eq!(body["provider"], "anthropic");
    assert_eq!(body["status"], "skip");
}

#[tokio::test]
async fn test_model_unqualified_known_alias_requires_a_ready_provider() {
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        |_| None,
    );
    let app = crate::test_support::build_test_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/models/sonnet/test"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_status!(response, StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn model_api_keeps_duplicate_ids_provider_scoped_and_selects_ready_priority() {
    let direct_upstream = MockServer::start();
    let aggregator_upstream = MockServer::start();
    let direct_probe = direct_upstream.mock(|when, then| {
        when.method(POST)
            .path("/v1/chat/completions")
            .json_body_includes(r#"{"model":"portable-model"}"#);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "id": "chatcmpl-direct",
                "model": "portable-model",
                "choices": [{
                    "message": {"role": "assistant", "content": "OK"},
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2
                }
            }));
    });
    let aggregator_probe = aggregator_upstream.mock(|when, then| {
        when.method(POST)
            .path("/v1/chat/completions")
            .json_body_includes(r#"{"model":"vendor/portable-model"}"#);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "id": "chatcmpl-aggregator",
                "model": "vendor/portable-model",
                "choices": [{
                    "message": {"role": "assistant", "content": "OK"},
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2
                }
            }));
    });
    let overlay = format!(
        r#"
[providers.direct]
display_name = "Direct"
base_url = {direct}
auth = {{ type = "bearer" }}
priority = 120
default_model = "portable-model"

[providers.direct.metadata.agent]
profile = "openai"

[providers.direct.models.portable-model]
display_name = "Portable (direct)"
aliases = ["portable"]
api_model = "portable-model"
limits = {{ context_tokens = 1000, max_output_tokens = 500 }}
capabilities = {{ text = true }}

[providers.aggregator]
display_name = "Aggregator"
base_url = {aggregator}
auth = {{ type = "bearer" }}
priority = 110
default_model = "portable-model"

[providers.aggregator.metadata.agent]
profile = "openai"

[providers.aggregator.models.portable-model]
display_name = "Portable (aggregator)"
aliases = ["portable"]
api_model = "vendor/portable-model"
limits = {{ context_tokens = 1000, max_output_tokens = 500 }}
capabilities = {{ text = true }}
"#,
        direct = toml::Value::String(direct_upstream.base_url()),
        aggregator = toml::Value::String(aggregator_upstream.base_url()),
    );
    let state = TestAppStateBuilder::new()
        .llm_overlay_toml(&overlay)
        .vault_entries([
            ("DIRECT_API_KEY", "direct-test-key"),
            ("AGGREGATOR_API_KEY", "aggregator-test-key"),
        ])
        .build();
    let app = crate::test_support::build_test_router(state);

    let list = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api("/models?query=portable-model"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list = response_json!(list, StatusCode::OK).await;
    let rows = list["data"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .map(|row| row["provider"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from(["aggregator", "direct"])
    );
    assert!(
        rows.iter()
            .all(|row| row["id"] == "portable-model" && row["configured"] == true)
    );

    let filtered = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api("/models?provider=aggregator&query=portable-model"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let filtered = response_json!(filtered, StatusCode::OK).await;
    assert_eq!(filtered["data"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["data"][0]["provider"], "aggregator");

    for (query, expected_provider) in [
        ("?provider=direct", "direct"),
        ("?provider=aggregator", "aggregator"),
        ("", "direct"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(api(&format!("/models/portable/test{query}")))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response_json!(response, StatusCode::OK).await;
        assert_eq!(body["model_id"], "portable-model");
        assert_eq!(body["provider"], expected_provider);
        assert_eq!(body["status"], "ok");
    }

    direct_probe.assert_calls(2);
    aggregator_probe.assert_calls(1);
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
async fn test_model_invalid_reasoning_effort_returns_400() {
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        |_| None,
    );
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/models/claude-opus-4-6/test?reasoning_effort=bogus"))
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn test_model_forwards_and_validates_reasoning_effort() {
    let upstream = MockServer::start();
    let completion = upstream.mock(|when, then| {
        when.method(POST)
            .path("/v1/chat/completions")
            .json_body_includes(r#"{"model":"acme-reasoner","reasoning_effort":"low"}"#);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "id": "chatcmpl-test",
                "model": "acme-reasoner",
                "choices": [{
                    "message": {"role": "assistant", "content": "OK"},
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2
                }
            }));
    });
    let overlay = format!(
        r#"
[providers.acme]
display_name = "Acme"
base_url = {base_url}
auth = {{ type = "bearer" }}
priority = 120
default_model = "acme-reasoner"

[providers.acme.metadata.agent]
profile = "openai"

[providers.acme.models.acme-reasoner]
display_name = "Acme Reasoner"
api_model = "acme-reasoner"
limits = {{ context_tokens = 128000, max_output_tokens = 8192 }}
capabilities = {{ text = true, tools = true, reasoning = true, reasoning_effort = {{ minimal = false, low = true, medium = false, high = true, xhigh = false, max = false }} }}
protocol_options = {{ reasoning_effort_levels = true }}
"#,
        base_url = toml::Value::String(upstream.base_url()),
    );
    let state = TestAppStateBuilder::new()
        .llm_overlay_toml(&overlay)
        .vault_entries([("ACME_API_KEY", "acme-test-key")])
        .build();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api(
            "/models/acme-reasoner/test?provider=acme&reasoning_effort=low",
        ))
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["status"], "ok");

    let unsupported = Request::builder()
        .method("POST")
        .uri(api(
            "/models/acme-reasoner/test?provider=acme&reasoning_effort=medium",
        ))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(unsupported).await.unwrap();
    let body = response_json!(response, StatusCode::BAD_REQUEST).await;
    assert_eq!(
        body["errors"][0]["detail"],
        "model 'acme-reasoner' does not support reasoning_effort 'medium'; allowed values: low, high"
    );
    completion.assert_calls(1);
}

#[tokio::test]
async fn test_provider_credentials_uses_app_state_catalog() {
    let upstream = MockServer::start();
    let completion = upstream.mock(|when, then| {
        when.method(POST)
            .path("/v1/chat/completions")
            .header("authorization", "Bearer sk-test");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "id": "chatcmpl_test",
                "object": "chat.completion",
                "created": 1_700_000_000,
                "model": "test-model",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "OK"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            }));
    });
    let overlay = acme_overlay(&upstream.base_url());
    let state = TestAppStateBuilder::new()
        .runtime_settings(default_test_server_settings(), RunLayer::default())
        .max_concurrent_runs(5)
        .llm_overlay_toml(&overlay)
        .build();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/providers/acme/credentials/test"))
        .header("content-type", "application/json")
        .body(Body::from(json!({ "api_key": "sk-test" }).to_string()))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["ok"], true);
    completion.assert();
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
async fn list_models_exposes_reasoning_effort_controls() {
    let app = test_app_with();

    let req = Request::builder()
        .method("GET")
        .uri(api("/models?provider=moonshot"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let models = body["data"].as_array().unwrap();
    let kimi_k3 = models
        .iter()
        .find(|model| model["id"] == "kimi-k3")
        .expect("Kimi K3 should be listed");
    let kimi_k2_5 = models
        .iter()
        .find(|model| model["id"] == "kimi-k2.5")
        .expect("Kimi K2.5 should be listed");

    assert_eq!(
        kimi_k3["controls"]["reasoning_effort"],
        json!(["low", "high", "max"])
    );
    assert_eq!(kimi_k2_5["controls"]["reasoning_effort"], json!([]));
}

#[tokio::test]
async fn list_models_marks_configured_true_when_provider_has_credential_material() {
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        |_| None,
    );
    state
        .stores
        .vault
        .set(
            EnvVars::ANTHROPIC_API_KEY,
            "test-key",
            SecretType::Token,
            None,
        )
        .await
        .unwrap();
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
    let overlay = acme_overlay("https://api.acme.test/v1");
    let state = TestAppStateBuilder::new()
        .runtime_settings(default_test_server_settings(), RunLayer::default())
        .max_concurrent_runs(5)
        .env_lookup(|name| (name == "ACME_API_KEY").then(|| "acme-key".to_string()))
        .llm_overlay_toml(&overlay)
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
    let overlay = acme_overlay("https://api.acme.test/v1");
    let state = TestAppStateBuilder::new()
        .llm_overlay_toml(&overlay)
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
    // Only `ANTHROPIC_API_KEY` is supplied in the vault, so anthropic resolves as
    // configured while every other catalog provider does not.
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        |_| None,
    );
    state
        .stores
        .vault
        .set(
            EnvVars::ANTHROPIC_API_KEY,
            "test-key",
            SecretType::Token,
            None,
        )
        .await
        .unwrap();
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
    let catalog = state_test_catalog();
    let anthropic_provider = catalog
        .enabled_provider("anthropic")
        .expect("anthropic should be listed");
    let expected_model_count = anthropic_provider.offerings().len();
    assert_eq!(
        anthropic["model_count"].as_u64(),
        Some(expected_model_count as u64),
        "anthropic model_count should match the catalog"
    );
    let expected_default = anthropic_provider
        .default_offering()
        .expect("anthropic should have a catalog default model");
    assert_eq!(
        anthropic["default_model"].as_str(),
        Some(expected_default.model.id().as_str()),
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
async fn test_providers_no_configured_providers_returns_error_summary() {
    let state = test_app_state_with_env_lookup(
        default_test_server_settings(),
        RunLayer::default(),
        5,
        |_| None,
    );
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/providers/test"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;

    assert_eq!(body["data"].as_array().unwrap().len(), 0);
    assert_eq!(body["summary"]["status"], "error");
    assert_eq!(body["summary"]["total"], 0);
    assert_eq!(body["summary"]["passed"], 0);
    assert_eq!(body["summary"]["failed"], 0);
}

#[tokio::test]
async fn test_providers_successful_probe_returns_probe_model() {
    let server = MockServer::start_async().await;
    let response_mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/responses")
                .header("authorization", "Bearer vault-openai-key");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(openai_responses_payload("OK"));
        })
        .await;
    let state = TestAppStateBuilder::new()
        .runtime_settings(default_test_server_settings(), RunLayer::default())
        .max_concurrent_runs(5)
        .provider_base_url("openai", server.url("/v1"))
        .build();
    state
        .stores
        .vault
        .set(
            EnvVars::OPENAI_API_KEY,
            "vault-openai-key",
            SecretType::Token,
            None,
        )
        .await
        .unwrap();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/providers/test"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let results = body["data"].as_array().unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["provider"], "openai");
    assert_eq!(results[0]["model_id"], "gpt-5.4-mini");
    assert_eq!(results[0]["status"], "ok");
    assert!(results[0]["error_message"].is_null());
    assert_eq!(body["summary"]["status"], "ok");
    assert_eq!(body["summary"]["total"], 1);
    assert_eq!(body["summary"]["passed"], 1);
    assert_eq!(body["summary"]["failed"], 0);
    response_mock.assert_async().await;
}

#[tokio::test]
async fn test_providers_auth_issue_returns_error_without_upstream_call() {
    let server = MockServer::start_async().await;
    let upstream = server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/responses");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(openai_responses_payload("unexpected"));
        })
        .await;
    let state = TestAppStateBuilder::new()
        .runtime_settings(default_test_server_settings(), RunLayer::default())
        .max_concurrent_runs(5)
        .provider_base_url("openai", server.url("/v1"))
        .build();
    let mut credential = openai_oauth_credential();
    credential.tokens.expires_at = Utc::now() - ChronoDuration::hours(1);
    credential.tokens.refresh_token = None;
    state
        .stores
        .vault
        .set(
            "OPENAI_CODEX",
            &serde_json::to_string(&credential).unwrap(),
            SecretType::Oauth,
            None,
        )
        .await
        .unwrap();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/providers/test"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let results = body["data"].as_array().unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["provider"], "openai-codex");
    assert!(results[0]["model_id"].is_null());
    assert_eq!(results[0]["status"], "error");
    assert!(
        results[0]["error_message"]
            .as_str()
            .unwrap()
            .contains("requires re-authentication")
    );
    assert_eq!(body["summary"]["status"], "error");
    assert_eq!(body["summary"]["total"], 1);
    assert_eq!(body["summary"]["passed"], 0);
    assert_eq!(body["summary"]["failed"], 1);
    upstream.assert_calls_async(0).await;
}

#[tokio::test]
async fn test_providers_registration_issue_returns_error_without_probe() {
    // An adapter lithos does not ship cannot be built, so the provider is
    // configured (it has a vault key) yet unavailable.
    let overlay = acme_overlay("https://api.acme.test/v1").replace(
        "display_name = \"Acme\"",
        "display_name = \"Acme\"\nadapter = \"not-an-adapter\"",
    );
    let state = TestAppStateBuilder::new()
        .runtime_settings(default_test_server_settings(), RunLayer::default())
        .max_concurrent_runs(5)
        .llm_overlay_toml(&overlay)
        .build();
    state
        .stores
        .vault
        .set("ACME_API_KEY", "acme-key", SecretType::Token, None)
        .await
        .unwrap();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/providers/test"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let results = body["data"].as_array().unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["provider"], "acme");
    assert!(results[0]["model_id"].is_null());
    assert_eq!(results[0]["status"], "error");
    assert!(
        results[0]["error_message"]
            .as_str()
            .unwrap()
            .contains("not-an-adapter")
    );
    assert_eq!(body["summary"]["status"], "error");
    assert_eq!(body["summary"]["total"], 1);
    assert_eq!(body["summary"]["passed"], 0);
    assert_eq!(body["summary"]["failed"], 1);
}

#[tokio::test]
async fn test_providers_mixed_results_preserve_catalog_order_and_counts() {
    let server = MockServer::start_async().await;
    let alpha_mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/responses")
                .header("authorization", "Bearer alpha-key");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(openai_responses_payload("OK"));
        })
        .await;
    let zeta_mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/responses")
                .header("authorization", "Bearer zeta-key");
            then.status(401)
                .header("content-type", "application/json")
                .json_body(json!({
                    "error": {
                        "message": "invalid api key",
                        "type": "invalid_request_error"
                    }
                }));
        })
        .await;
    let overlay = format!(
        r#"
[providers.zeta]
display_name = "Zeta"
codecs = ["openai-responses"]
base_url = {base_url}
auth = {{ type = "bearer" }}
priority = 50
default_model = "zeta-probe"

[providers.zeta.models.zeta-probe]
display_name = "Zeta Probe"
api_model = "zeta-probe"
limits = {{ context_tokens = 128000, max_output_tokens = 8192 }}
capabilities = {{ text = true, tools = true }}
probe = true

[providers.alpha]
display_name = "Alpha"
codecs = ["openai-responses"]
base_url = {base_url}
auth = {{ type = "bearer" }}
priority = 40
default_model = "alpha-probe"

[providers.alpha.models.alpha-probe]
display_name = "Alpha Probe"
api_model = "alpha-probe"
limits = {{ context_tokens = 128000, max_output_tokens = 8192 }}
capabilities = {{ text = true, tools = true }}
probe = true

"#,
        base_url = toml::Value::String(server.base_url()),
    );
    let state = TestAppStateBuilder::new()
        .runtime_settings(default_test_server_settings(), RunLayer::default())
        .max_concurrent_runs(5)
        .llm_overlay_toml(&overlay)
        .build();
    state
        .stores
        .vault
        .set("ALPHA_API_KEY", "alpha-key", SecretType::Token, None)
        .await
        .unwrap();
    state
        .stores
        .vault
        .set("ZETA_API_KEY", "zeta-key", SecretType::Token, None)
        .await
        .unwrap();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/providers/test"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let results = body["data"].as_array().unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["provider"], "alpha");
    assert_eq!(results[0]["model_id"], "alpha-probe");
    assert_eq!(results[0]["status"], "ok");
    assert_eq!(results[1]["provider"], "zeta");
    assert_eq!(results[1]["model_id"], "zeta-probe");
    assert_eq!(results[1]["status"], "error");
    assert_eq!(body["summary"]["status"], "error");
    assert_eq!(body["summary"]["total"], 2);
    assert_eq!(body["summary"]["passed"], 1);
    assert_eq!(body["summary"]["failed"], 1);
    alpha_mock.assert_async().await;
    zeta_mock.assert_async().await;
}

#[tokio::test]
async fn test_providers_response_does_not_leak_api_keys() {
    let leaked_key = "sk-proj-abcdefghijklmnopqrstuvwxyz0123456789";
    let server = MockServer::start_async().await;
    let response_mock = server
        .mock_async(move |when, then| {
            when.method(POST)
                .path("/v1/responses")
                .header("authorization", format!("Bearer {leaked_key}"));
            then.status(401)
                .header("content-type", "application/json")
                .json_body(json!({
                    "error": {
                        "message": format!("invalid api key {leaked_key}"),
                        "type": "invalid_request_error"
                    }
                }));
        })
        .await;
    let state = TestAppStateBuilder::new()
        .runtime_settings(default_test_server_settings(), RunLayer::default())
        .max_concurrent_runs(5)
        .provider_base_url("openai", server.url("/v1"))
        .build();
    state
        .stores
        .vault
        .set(EnvVars::OPENAI_API_KEY, leaked_key, SecretType::Token, None)
        .await
        .unwrap();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/providers/test"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    let serialized = body.to_string();

    assert!(
        !serialized.contains(leaked_key),
        "provider test response leaked API key: {serialized}"
    );
    assert!(
        serialized.contains("REDACTED"),
        "provider test response should include a redacted error: {serialized}"
    );
    response_mock.assert_async().await;
}

#[tokio::test]
async fn test_providers_requires_user_auth() {
    let app = build_router(test_app_state(), test_auth_mode());

    let req = Request::builder()
        .method("POST")
        .uri(api("/providers/test"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::UNAUTHORIZED).await;
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
        .body(intent_body(&app, MINIMAL_DOT).await)
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
            options:         vec![fabro_types::InterviewOption {
                key:         "approve".to_string(),
                label:       "Approve".to_string(),
                description: None,
                preview:     None,
            }],
            allow_freeform:  false,
            timeout_seconds: None,
            context_display: None,
            review_target:   None,
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
        review_target:   None,
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
        review_target:   None,
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
        review_target:   None,
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
        options:         vec![fabro_types::InterviewOption {
            key:         "approve".to_string(),
            label:       "Approve".to_string(),
            description: None,
            preview:     None,
        }],
        allow_freeform:  false,
        timeout_seconds: None,
        context_display: None,
        review_target:   None,
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
            fabro_types::InterviewOption {
                key:         "approve".to_string(),
                label:       "Approve".to_string(),
                description: None,
                preview:     None,
            },
            fabro_types::InterviewOption {
                key:         "notify".to_string(),
                label:       "Notify".to_string(),
                description: None,
                preview:     None,
            },
        ],
        allow_freeform:  false,
        timeout_seconds: None,
        context_display: None,
        review_target:   None,
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
        .body(intent_body(&app, MINIMAL_DOT).await)
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
    let records = platform_records(&state, run_id).await;
    assert!(records.iter().any(|stored| {
        matches!(
            &stored.record,
            PlatformRecord::PullRequestUnlinked(unlinked)
                if unlinked.link().html_url() == "https://github.com/acme/widgets/pull/42"
        )
    }));
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
async fn get_run_state_includes_provenance_from_user_agent() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .header("user-agent", "fabro-cli/1.2.3")
        .body(intent_body(&app, MINIMAL_DOT).await)
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
                .body(Body::from(
                    test_intent_with_bearer(
                        &app,
                        "workflow.fabro",
                        MINIMAL_DOT,
                        None,
                        Some(DEV_TOKEN),
                    )
                    .await
                    .to_string(),
                ))
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
async fn list_run_events_returns_paginated_json() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(intent_body(&app, MINIMAL_DOT).await)
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
async fn write_and_read_run_blob_accepts_uppercase_hash() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(intent_body(&app, MINIMAL_DOT).await)
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
    let blob_hash = body["hash"].as_str().unwrap();

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!(
            "/runs/{run_id}/blobs/{}",
            blob_hash.to_uppercase()
        )))
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
    seed_stage_artifact(&state, &run_id, stage_id, 1, "src/lib.rs", b"fn main() {}").await;

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
        seed_stage_artifact(
            &state,
            &run_id,
            stage_id,
            retry,
            "logs/output.txt",
            body.as_bytes(),
        )
        .await;
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
async fn run_artifacts_download_returns_not_found_for_unknown_run() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api(&format!("/runs/{}/artifacts/download", RunId::new())))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn create_run_persists_run_spec() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let run_id = create_run(&app, MINIMAL_DOT)
        .await
        .parse::<RunId>()
        .unwrap();
    let run_state = state.load_run_projection(&run_id).await.unwrap();

    assert_eq!(run_state.spec.graph.name, "Test");
}

#[tokio::test]
async fn create_run_keeps_missing_project_and_workflow_names_absent() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let intent = test_intent_with_bearer(
        &app,
        "workflow.fabro",
        "digraph Demo { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
        Some("_version = 1\n"),
        None,
    )
    .await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/runs"))
                .header("content-type", "application/json")
                .body(Body::from(intent.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap().parse::<RunId>().unwrap();

    let run_state = state.load_run_projection(&run_id).await.unwrap();

    assert_eq!(run_state.spec.settings.project.name.as_deref(), None);
    assert_eq!(run_state.spec.settings.workflow.name.as_deref(), None);
    assert_eq!(run_state.spec.graph_name(), Some("Demo"));
}

#[tokio::test]
async fn worker_token_accepts_run_scoped_routes_and_falls_back_to_user_jwt() {
    let (state, app) = jwt_auth_app();
    let user_jwt = issue_test_user_jwt();
    let run_id = create_run_with_bearer(&app, &user_jwt).await;
    let worker_token = issue_test_worker_token(&run_id);
    let other_run_id = create_run_with_bearer(&app, &user_jwt).await;
    let other_worker_token = issue_test_worker_token(&other_run_id);
    let blob_hash = state
        .store_ref()
        .blobs()
        .write(b"preloaded blob")
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
            &format!("/runs/{run_id}/blobs/{blob_hash}"),
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
    let projection = state
        .stores
        .runs
        .load_run_projection(&created_child)
        .await
        .unwrap()
        .expect("created run should have a projection");
    assert_eq!(projection.spec.provenance.subject, Principal::Worker {
        run_id: parent_run_id,
    },);

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
async fn cross_run_base_worker_remains_forbidden_from_pair_routes() {
    let (_state, app) = jwt_auth_app();
    let user_jwt = issue_test_user_jwt();
    let origin_run_id = create_run_with_bearer(&app, &user_jwt).await;
    let target_run_id = create_run_with_bearer(&app, &user_jwt).await;
    let worker_token = issue_test_worker_token(&origin_run_id);

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/runs/{target_run_id}/pair"),
            &worker_token,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::FORBIDDEN).await;
}

#[tokio::test]
async fn run_tools_worker_cannot_call_user_only_non_mcp_routes() {
    let (_state, app) = jwt_auth_app();
    let user_jwt = issue_test_user_jwt();
    let origin_run_id = create_run_with_bearer(&app, &user_jwt).await;
    let target_run_id = create_run_with_bearer(&app, &user_jwt).await;
    let worker_token = issue_test_run_tools_worker_token(&origin_run_id);

    for (method, path) in [
        (Method::POST, format!("/runs/{target_run_id}/approve")),
        (Method::POST, format!("/runs/{target_run_id}/deny")),
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
            "{method} {path} unexpectedly accepted run-tools worker token with status {}",
            response.status()
        );
    }
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
async fn worker_token_is_rejected_on_user_only_routes() {
    let (_state, app) = jwt_auth_app();
    let user_jwt = issue_test_user_jwt();
    let run_id = create_run_with_bearer(&app, &user_jwt).await;
    let worker_token = issue_test_worker_token(&run_id);
    let blob_hash = BlobHash::new(b"blob");
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
        (Method::POST, format!("/runs/{run_id}/pause")),
        (Method::POST, format!("/runs/{run_id}/unpause")),
        (Method::GET, format!("/runs/{run_id}/graph")),
        (Method::GET, format!("/runs/{run_id}/graph/source")),
        (Method::GET, format!("/runs/{run_id}/stages")),
        (Method::GET, format!("/runs/{run_id}/artifacts")),
        (Method::GET, format!("/runs/{run_id}/artifacts/download")),
        (Method::GET, format!("/runs/{run_id}/files")),
        (
            Method::GET,
            format!("/runs/{run_id}/stages/code@2/artifacts"),
        ),
        (
            Method::GET,
            format!("/runs/{run_id}/stages/code@2/artifacts/download"),
        ),
        (Method::GET, format!("/runs/{run_id}/usage")),
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
            &format!("/runs/{run_id}/blobs/{blob_hash}"),
            &worker_token,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_run_accepts_explicit_title() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let mut intent = test_intent(&app, MINIMAL_DOT).await;
    intent["title"] = json!("  Explicit server title  ");

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&intent).unwrap()))
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
        let mut intent = test_intent(&app, MINIMAL_DOT).await;
        intent["title"] = json!(title);
        let req = Request::builder()
            .method("POST")
            .uri(api("/runs"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&intent).unwrap()))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_status!(response, StatusCode::UNPROCESSABLE_ENTITY).await;
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
        .body(intent_body(&app, MINIMAL_DOT).await)
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
        .load_run_projection(&run_id.parse::<RunId>().unwrap())
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
    let mut child_intent =
        test_intent_with_bearer(&app, "workflow.fabro", MINIMAL_DOT, None, Some(&user_jwt)).await;
    child_intent["parent_id"] = json!(parent_run_id.to_string());

    let response = app
        .clone()
        .oneshot(json_bearer_request(
            Method::POST,
            "/runs",
            &worker_token,
            &child_intent,
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

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            "/system/info",
            &user_jwt,
            Body::empty(),
        ))
        .await
        .unwrap();
    let info_body = response_json!(response, StatusCode::OK).await;
    assert_eq!(info_body["runs"]["active"], 1);
    assert_eq!(info_body["runs"]["scheduler_slots_used"], 0);

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
    let mut child_intent =
        test_intent_with_bearer(&app, "workflow.fabro", MINIMAL_DOT, None, Some(&user_jwt)).await;
    child_intent["parent_id"] = json!(parent_run_id.to_string());

    let response = app
        .clone()
        .oneshot(json_bearer_request(
            Method::POST,
            "/runs",
            &worker_token,
            &child_intent,
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
        .body(intent_body(&app, MINIMAL_DOT).await)
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
    let (transport, mut control_rx) = worker_transport_with_receiver(run_id).await;
    let _temp_dir = insert_running_control_run(&state, run_id, Some(transport));

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/steer")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"try again"}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::ACCEPTED).await;
    let envelope = recv_worker_control_envelope(&mut control_rx).await;
    assert!(matches!(
        envelope.message,
        WorkerControlMessage::Steer { ref text, .. } if text == "try again"
    ));
}

#[tokio::test]
async fn steer_with_a_stage_forwards_the_stage_to_the_worker() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let (transport, mut control_rx) = worker_transport_with_receiver(run_id).await;
    let _temp_dir = insert_running_control_run(&state, run_id, Some(transport));

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/steer")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"try again","stage":"code@2"}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::ACCEPTED).await;
    let envelope = recv_worker_control_envelope(&mut control_rx).await;
    assert!(matches!(
        envelope.message,
        WorkerControlMessage::Steer { ref text, ref stage, .. }
            if text == "try again" && stage.as_deref() == Some("code@2")
    ));
}

#[tokio::test]
async fn steer_with_a_blank_stage_returns_bad_request() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let (transport, _control_rx) = worker_transport_with_receiver(run_id).await;
    let _temp_dir = insert_running_control_run(&state, run_id, Some(transport));

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/steer")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"try again","stage":"  "}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn steer_with_active_non_steerable_session_returns_conflict() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let stage_id = StageId::new("agent", 1);
    let (transport, _control_rx) = worker_transport_with_receiver(run_id).await;
    let _temp_dir = insert_running_control_run(&state, run_id, Some(transport));
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
async fn steer_with_interrupt_forwards_an_interrupt_then_steer_to_the_worker() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let (transport, mut control_rx) = worker_transport_with_receiver(run_id).await;
    let _temp_dir = insert_running_control_run(&state, run_id, Some(transport));

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/steer")))
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"text":"stop and summarize","interrupt":true,"stage":"code@2"}"#,
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::ACCEPTED).await;
    let envelope = recv_worker_control_envelope(&mut control_rx).await;
    assert!(
        matches!(
            envelope.message,
            WorkerControlMessage::InterruptThenSteer { ref text, ref stage, .. }
                if text == "stop and summarize" && stage.as_deref() == Some("code@2")
        ),
        "{envelope:?}"
    );
}

#[tokio::test]
async fn interrupt_forwards_the_stage_and_text_to_the_worker() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let (transport, mut control_rx) = worker_transport_with_receiver(run_id).await;
    let _temp_dir = insert_running_control_run(&state, run_id, Some(transport));

    // No body: the run's one live agent stage, waiting for the next steer.
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/interrupt")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::ACCEPTED).await;
    let envelope = recv_worker_control_envelope(&mut control_rx).await;
    assert!(
        matches!(envelope.message, WorkerControlMessage::Interrupt {
            stage: None,
            ..
        }),
        "{envelope:?}"
    );

    // A stage alone: a plain interrupt of that stage.
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/interrupt")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"stage":"code@2"}"#))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::ACCEPTED).await;
    let envelope = recv_worker_control_envelope(&mut control_rx).await;
    assert!(
        matches!(
            envelope.message,
            WorkerControlMessage::Interrupt { ref stage, .. } if stage.as_deref() == Some("code@2")
        ),
        "{envelope:?}"
    );

    // A stage and a text: the text is the stage's next input.
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/interrupt")))
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"stage":"code@2","text":"stop and summarize"}"#,
        ))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::ACCEPTED).await;
    let envelope = recv_worker_control_envelope(&mut control_rx).await;
    assert!(
        matches!(
            envelope.message,
            WorkerControlMessage::InterruptThenSteer { ref text, ref stage, .. }
                if text == "stop and summarize" && stage.as_deref() == Some("code@2")
        ),
        "{envelope:?}"
    );
}

#[tokio::test]
async fn interrupt_with_a_blank_stage_or_text_returns_bad_request() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let (transport, _control_rx) = worker_transport_with_receiver(run_id).await;
    let _temp_dir = insert_running_control_run(&state, run_id, Some(transport));

    for body in [r#"{"stage":"  "}"#, r#"{"text":"  "}"#] {
        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/interrupt")))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
    }
}

#[tokio::test]
async fn interrupt_of_a_blocked_run_is_forwarded_for_the_worker_to_judge() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let (transport, mut control_rx) = worker_transport_with_receiver(run_id).await;
    let _temp_dir = insert_running_control_run(&state, run_id, Some(transport));
    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        runs.get_mut(&run_id).unwrap().status = RunStatus::Blocked {
            blocked_reason: BlockedReason::HumanInputRequired,
        };
    }

    // A steer of a blocked run goes to the answer endpoint; an interrupt
    // may still name an agent stage running beside the question, so the
    // worker decides, and refuses a gate with `no_live_turn` on the stream.
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/steer")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"try again"}"#))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = body_json(response.into_body()).await;
    assert_eq!(body["errors"][0]["code"], "use_answer_endpoint");

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/interrupt")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"stage":"gate@1"}"#))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::ACCEPTED).await;
    let envelope = recv_worker_control_envelope(&mut control_rx).await;
    assert!(
        matches!(
            envelope.message,
            WorkerControlMessage::Interrupt { ref stage, .. } if stage.as_deref() == Some("gate@1")
        ),
        "{envelope:?}"
    );
}

#[tokio::test]
async fn interrupt_of_a_finished_run_returns_conflict() {
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

#[tokio::test]
async fn interrupt_of_an_unknown_run_returns_not_found() {
    let app = test_app_with();
    let missing_run_id = fixtures::RUN_64;

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{missing_run_id}/interrupt")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
}

/// A steer the worker delivers: 202 with the outcome and the stage.
#[tokio::test]
async fn steer_delivered_by_the_worker_returns_accepted_with_its_stage() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let (transport, control_rx, acks) = worker_transport_with_acks(run_id).await;
    let _temp_dir = insert_running_control_run(&state, run_id, Some(transport));
    let _seen = answering_worker(run_id, control_rx, acks, WorkerControlOutcome::Delivered {
        stage: Some("work@1".to_string()),
    });

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/steer")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"try again"}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = body_json(response.into_body()).await;
    assert_eq!(
        body,
        serde_json::json!({ "outcome": "delivered", "stage": "work@1" })
    );
}

/// A control the worker refuses: 409 with the refusal's code and reason,
/// for each code the worker can answer with.
#[tokio::test]
async fn control_refused_by_the_worker_returns_conflict_with_its_code() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let refusals = [
        (
            "steer",
            r#"{"text":"try again"}"#,
            "steer_refused",
            "Steer refused: Run has no active steerable agent session.",
        ),
        (
            "steer",
            r#"{"text":"try again","stage":"nope"}"#,
            "no_such_stage",
            "Steer of stage `nope` refused: no stage named `nope` is running",
        ),
        (
            "interrupt",
            r#"{"stage":"gate"}"#,
            "no_live_turn",
            "Interrupt of stage `gate` refused: the stage has no model turn to interrupt",
        ),
        (
            "interrupt",
            r#"{"stage":"nope"}"#,
            "no_such_stage",
            "Interrupt of stage `nope` refused: no stage named `nope` is running",
        ),
        (
            "interrupt",
            "{}",
            "interrupt_refused",
            "Interrupt refused: Run has no active steerable agent session.",
        ),
    ];
    for (index, (action, body, code, message)) in refusals.into_iter().enumerate() {
        let run_id = RunId::new();
        let (transport, control_rx, acks) = worker_transport_with_acks(run_id).await;
        let _temp_dir = insert_running_control_run(&state, run_id, Some(transport));
        let _seen = answering_worker(run_id, control_rx, acks, WorkerControlOutcome::Refused {
            code:    code.to_string(),
            message: message.to_string(),
        });

        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}/{action}")))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT, "refusal {index}");
        let response_body = body_json(response.into_body()).await;
        assert_eq!(response_body["errors"][0]["code"], code, "{response_body}");
        assert_eq!(
            response_body["errors"][0]["detail"], message,
            "{response_body}"
        );
    }
}

/// A worker that never answers: 202 `pending` once the wait runs out,
/// with the control still forwarded.
#[tokio::test]
async fn interrupt_unanswered_by_the_worker_returns_accepted_pending() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let (transport, mut control_rx) = worker_transport_with_receiver(run_id).await;
    let _temp_dir = insert_running_control_run(&state, run_id, Some(transport));

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/interrupt")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = body_json(response.into_body()).await;
    assert_eq!(body, serde_json::json!({ "outcome": "pending" }));
    let envelope = recv_worker_control_envelope(&mut control_rx).await;
    assert!(envelope.request_id().is_some(), "{envelope:?}");
}

#[tokio::test]
async fn interrupt_without_a_worker_channel_returns_unavailable() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = fixtures::RUN_1;
    let _temp_dir = insert_running_control_run(&state, run_id, None);

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/interrupt")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = body_json(response.into_body()).await;
    assert_eq!(body["errors"][0]["code"], "worker_control_unavailable");
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
            serde_json::to_string(&test_intent(&app, MINIMAL_DOT).await).unwrap(),
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
            serde_json::to_string(&test_intent(&app, MINIMAL_DOT).await).unwrap(),
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
        .body(intent_body(&app, MINIMAL_DOT).await)
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
    assert_eq!(items[0]["usage"]["tokens"]["input"], 0);
    assert!(items[0]["usage"].get("cost").is_none());
}

fn batch_lifecycle_body(run_ids: &[RunId]) -> serde_json::Value {
    json!({
        "run_ids": run_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
    })
}

fn batch_delete_body(run_ids: &[RunId], force: bool) -> serde_json::Value {
    json!({
        "run_ids": run_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "force": force,
    })
}

#[tokio::test]
async fn batch_lifecycle_requires_user_authentication() {
    let (_state, app) = jwt_auth_app();
    let user_jwt = issue_test_user_jwt();
    let run_id = create_run_with_bearer(&app, &user_jwt).await;
    let worker_token = issue_test_worker_token(&run_id);
    let body = batch_lifecycle_body(&[run_id]);

    let unauthenticated = app
        .clone()
        .oneshot(json_request(Method::POST, "/runs/archive", &body))
        .await
        .unwrap();
    assert_status!(unauthenticated, StatusCode::UNAUTHORIZED).await;

    for path in ["/runs/archive", "/runs/unarchive"] {
        let worker_response = app
            .clone()
            .oneshot(json_bearer_request(
                Method::POST,
                path,
                &worker_token,
                &body,
            ))
            .await
            .unwrap();
        assert!(
            matches!(
                worker_response.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ),
            "{path} unexpectedly accepted worker token with status {}",
            worker_response.status()
        );
    }
}

#[tokio::test]
async fn batch_delete_requires_user_authentication() {
    let (_state, app) = jwt_auth_app();
    let user_jwt = issue_test_user_jwt();
    let run_id = create_run_with_bearer(&app, &user_jwt).await;
    let worker_token = issue_test_worker_token(&run_id);
    let body = batch_delete_body(&[run_id], false);

    let unauthenticated = app
        .clone()
        .oneshot(json_request(Method::POST, "/runs/delete", &body))
        .await
        .unwrap();
    assert_status!(unauthenticated, StatusCode::UNAUTHORIZED).await;

    let worker_response = app
        .oneshot(json_bearer_request(
            Method::POST,
            "/runs/delete",
            &worker_token,
            &body,
        ))
        .await
        .unwrap();
    assert!(
        matches!(
            worker_response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ),
        "/runs/delete unexpectedly accepted worker token with status {}",
        worker_response.status()
    );
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
        .body(intent_body(&app, MINIMAL_DOT).await)
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
        .body(intent_body(&app, MINIMAL_DOT).await)
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
        .body(intent_body(&app, MINIMAL_DOT).await)
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
async fn get_aggregate_usage_returns_zeros_initially() {
    let state = test_app_state();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("GET")
        .uri(api("/usage"))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["totals"]["runs"].as_i64().unwrap(), 0);
    assert_eq!(
        body["totals"]["usage"]["tokens"]["input"].as_u64().unwrap(),
        0
    );
    assert_eq!(
        body["totals"]["usage"]["tokens"]["output"]
            .as_u64()
            .unwrap(),
        0
    );
    assert_eq!(
        body["totals"]["timing"]["wall_time_ms"].as_u64().unwrap(),
        0
    );
    assert!(body["totals"]["usage"].get("cost").is_none());
    assert!(body["by_model"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn get_aggregate_usage_returns_provider_model_speed_identity() {
    let state = test_app_state();
    {
        let mut agg = state.aggregate_usage.lock().expect("aggregate usage lock");
        agg.total_runs = 1;
        agg.by_model.insert(
            ModelRef::new(
                lithos_llm::catalog::builtin::anthropic(),
                ModelId::new("claude-opus-4-6"),
            ),
            ModelUsageTotals {
                stages: 1,
                usage:  test_priced_usage("claude-opus-4-6", 10, 1).usage,
            },
        );
        agg.by_model.insert(
            ModelRef::new(
                lithos_llm::catalog::builtin::anthropic(),
                ModelId::new("claude-opus-4-6"),
            )
            .with_speed(Some(Speed::Fast)),
            ModelUsageTotals {
                stages: 1,
                usage:  test_priced_usage("claude-opus-4-6", 20, 2).usage,
            },
        );
    }
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api("/usage"))
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
    assert_eq!(standard["usage"]["tokens"]["input"], 10);
    assert_eq!(fast["model"]["provider"], "anthropic");
    assert_eq!(fast["model"]["model_id"], "claude-opus-4-6");
    assert_eq!(fast["usage"]["tokens"]["input"], 20);
}

#[tokio::test]
async fn get_aggregate_usage_saturates_total_cost_across_models() {
    let state = test_app_state();
    {
        let mut agg = state.aggregate_usage.lock().expect("aggregate usage lock");
        for (model_id, usd_micros) in [("maximum", u64::MAX), ("one", 1)] {
            agg.by_model.insert(
                ModelRef::new(
                    lithos_llm::catalog::builtin::openai(),
                    ModelId::new(model_id),
                ),
                ModelUsageTotals {
                    stages: 1,
                    usage:  Usage {
                        tokens: TokenCounts::default(),
                        cost:   Some(Cost {
                            usd_micros,
                            source: CostSource::Catalog,
                        }),
                    },
                },
            );
        }
    }
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(api("/usage"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::OK).await;

    assert_eq!(
        body["totals"]["usage"]["cost"]["usd_micros"].as_u64(),
        Some(u64::MAX)
    );
}

#[test]
fn aggregate_usage_counts_projection_rollup_usage_visits() {
    let mut accumulator = UsageAccumulator::default();
    let rollup = fabro_types::usage_rollup::ProjectionUsageRollup {
        stages:            Vec::new(),
        totals:            test_priced_usage("gpt-5.4", 300, 30).usage,
        by_model:          vec![
            fabro_types::usage_rollup::ProjectionUsageByModel {
                model:  ModelRef::new(
                    lithos_llm::catalog::builtin::openai(),
                    ModelId::new("gpt-5.4"),
                ),
                stages: 1,
                usage:  test_priced_usage("gpt-5.4", 100, 10).usage,
            },
            fabro_types::usage_rollup::ProjectionUsageByModel {
                model:  ModelRef::new(
                    lithos_llm::catalog::builtin::openai(),
                    ModelId::new("gpt-5.4"),
                )
                .with_speed(Some(Speed::Fast)),
                stages: 1,
                usage:  test_priced_usage("gpt-5.4", 200, 20).usage,
            },
        ],
        timing:            fabro_types::RunTiming::wall_only(2000),
        usage_visit_count: 2,
    };

    accumulate_usage_rollup(&mut accumulator, &rollup);

    assert_eq!(accumulator.total_runs, 1);
    assert_eq!(accumulator.total_timing.wall_time_ms, 2000);
    assert_eq!(accumulator.by_model.len(), 2);
    assert_eq!(
        accumulator.by_model[&ModelRef::new(
            lithos_llm::catalog::builtin::openai(),
            ModelId::new("gpt-5.4")
        )]
            .stages,
        1
    );
    assert_eq!(
        accumulator.by_model[&ModelRef::new(
            lithos_llm::catalog::builtin::openai(),
            ModelId::new("gpt-5.4")
        )]
            .usage
            .tokens
            .input,
        100
    );
    assert_eq!(
        accumulator.by_model[&ModelRef::new(
            lithos_llm::catalog::builtin::openai(),
            ModelId::new("gpt-5.4")
        )
        .with_speed(Some(Speed::Fast))]
            .stages,
        1
    );
    assert_eq!(
        accumulator.by_model[&ModelRef::new(
            lithos_llm::catalog::builtin::openai(),
            ModelId::new("gpt-5.4")
        )
        .with_speed(Some(Speed::Fast))]
            .usage
            .tokens
            .input,
        200
    );
}

#[expect(
    clippy::disallowed_methods,
    reason = "test asserts the raw template source"
)]
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
    state
        .stores
        .vault
        .set(
            EnvVars::ANTHROPIC_API_KEY,
            "test-anthropic-api-key",
            SecretType::Token,
            None,
        )
        .await
        .unwrap();
    let app = crate::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(intent_body(&app, MINIMAL_DOT).await)
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
    let projection = state.load_run_projection(&run_id).await.unwrap();
    let run_spec = &projection.spec;
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
        resolved_run.execution.mode == RunMode::DryRun,
        "run execution mode should inherit from server settings"
    );
    // The snapshot keeps the configured name: Petri resolved the model at
    // admission and pinned it in the admitted graph, not in the settings.
    assert_eq!(
        resolved_run.model.name.as_deref(),
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

    let status = state.load_run_projection(&run_id).await.unwrap().status;
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
        managed_run.worker_ref = Some(test_worker_ref(u32::MAX));
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
    let body = response_json!(response, StatusCode::ACCEPTED).await;
    assert_eq!(run_json_pending_control(&body).as_str(), Some("cancel"));

    let summary = state
        .stores
        .run_summaries
        .get(&run_id, Utc::now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        summary.lifecycle.pending_control,
        Some(RunControlAction::Cancel)
    );
}

async fn advance_past_worker_cancel_grace() {
    tokio::task::yield_now().await;
    tokio::time::advance(WORKER_CANCEL_GRACE).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn cancel_run_requests_worker_runtime_stop_when_control_unavailable() {
    let runtime = StdArc::new(RecordingWorkerRuntime::default());
    runtime.set_alive(true);
    let state = TestAppStateBuilder::new()
        .vault_entries([(EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
        .worker_runtime(runtime.clone())
        .build();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = create_run(&app, MINIMAL_DOT)
        .await
        .parse::<RunId>()
        .unwrap();
    let worker_ref = test_worker_ref(u32::MAX);

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.status = RunStatus::Running;
        managed_run.answer_transport = None;
        managed_run.worker_ref = Some(worker_ref.clone());
    }

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/cancel")))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::ACCEPTED).await;

    assert_eq!(runtime.requested_refs(), vec![worker_ref.clone()]);

    tokio::time::pause();
    advance_past_worker_cancel_grace().await;
    runtime.wait_for_forced_ref(&worker_ref).await;

    assert_eq!(runtime.forced_refs(), vec![worker_ref]);
}

#[tokio::test]
async fn cancel_run_force_stops_worker_when_delivered_control_does_not_converge() {
    let runtime = StdArc::new(RecordingWorkerRuntime::default());
    runtime.set_alive(true);
    let state = TestAppStateBuilder::new()
        .vault_entries([(EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
        .worker_runtime(runtime.clone())
        .build();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = create_run(&app, MINIMAL_DOT)
        .await
        .parse::<RunId>()
        .unwrap();
    let worker_ref = test_worker_ref(u32::MAX);
    let (answer_transport, _receiver) = worker_transport_with_receiver(run_id).await;

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.status = RunStatus::Running;
        managed_run.answer_transport = Some(answer_transport);
        managed_run.worker_ref = Some(worker_ref.clone());
    }

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/cancel")))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::ACCEPTED).await;

    assert!(runtime.requested_refs().is_empty());
    assert!(runtime.forced_refs().is_empty());

    tokio::time::pause();
    advance_past_worker_cancel_grace().await;
    runtime.wait_for_forced_ref(&worker_ref).await;

    assert_eq!(runtime.forced_refs(), vec![worker_ref]);
}

#[tokio::test]
async fn cancel_run_watchdog_does_not_stop_replacement_worker() {
    let runtime = StdArc::new(RecordingWorkerRuntime::default());
    runtime.set_alive(true);
    let state = TestAppStateBuilder::new()
        .vault_entries([(EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
        .worker_runtime(runtime.clone())
        .build();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = create_run(&app, MINIMAL_DOT)
        .await
        .parse::<RunId>()
        .unwrap();
    let cancelled_worker_ref = test_worker_ref(u32::MAX - 1);
    let replacement_worker_ref = test_worker_ref(u32::MAX);
    let (answer_transport, _receiver) = worker_transport_with_receiver(run_id).await;

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.status = RunStatus::Running;
        managed_run.answer_transport = Some(answer_transport);
        managed_run.worker_ref = Some(cancelled_worker_ref);
    }

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/cancel")))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::ACCEPTED).await;

    tokio::time::pause();
    tokio::task::yield_now().await;
    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.worker_ref = Some(replacement_worker_ref);
    }
    advance_past_worker_cancel_grace().await;

    assert!(runtime.forced_refs().is_empty());
}

#[tokio::test]
async fn cancel_run_watchdog_does_not_stop_worker_after_live_ref_clears() {
    let runtime = StdArc::new(RecordingWorkerRuntime::default());
    runtime.set_alive(true);
    let state = TestAppStateBuilder::new()
        .vault_entries([(EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
        .worker_runtime(runtime.clone())
        .build();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = create_run(&app, MINIMAL_DOT)
        .await
        .parse::<RunId>()
        .unwrap();
    let worker_ref = test_worker_ref(u32::MAX);
    let (answer_transport, _receiver) = worker_transport_with_receiver(run_id).await;

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.status = RunStatus::Running;
        managed_run.answer_transport = Some(answer_transport);
        managed_run.worker_ref = Some(worker_ref);
    }

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/cancel")))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_status!(response, StatusCode::ACCEPTED).await;

    tokio::time::pause();
    tokio::task::yield_now().await;
    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.worker_ref = None;
    }
    advance_past_worker_cancel_grace().await;

    assert!(runtime.forced_refs().is_empty());
}

#[tokio::test]
async fn repeated_cancel_request_arms_one_watchdog_and_persists_one_intent() {
    let runtime = StdArc::new(RecordingWorkerRuntime::default());
    runtime.set_alive(true);
    let state = TestAppStateBuilder::new()
        .vault_entries([(EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
        .worker_runtime(runtime.clone())
        .build();
    let app = crate::test_support::build_test_router(Arc::clone(&state));
    let run_id = create_run(&app, MINIMAL_DOT)
        .await
        .parse::<RunId>()
        .unwrap();
    let worker_ref = test_worker_ref(u32::MAX);
    let (answer_transport, _receiver) = worker_transport_with_receiver(run_id).await;

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.status = RunStatus::Running;
        managed_run.answer_transport = Some(answer_transport);
        managed_run.worker_ref = Some(worker_ref.clone());
    }

    let first_request = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/cancel")))
        .body(Body::empty())
        .unwrap();
    let second_request = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/cancel")))
        .body(Body::empty())
        .unwrap();
    let (first_response, second_response) = tokio::join!(
        app.clone().oneshot(first_request),
        app.clone().oneshot(second_request)
    );
    assert_status!(first_response.unwrap(), StatusCode::ACCEPTED).await;
    assert_status!(second_response.unwrap(), StatusCode::ACCEPTED).await;

    let request_count = lifecycle_transition_count(
        &platform_records(&state, run_id).await,
        RunLifecycleKind::CancelRequested,
    );
    assert_eq!(request_count, 1);

    tokio::time::pause();
    advance_past_worker_cancel_grace().await;
    runtime.wait_for_forced_ref(&worker_ref).await;

    assert_eq!(runtime.forced_refs(), vec![worker_ref]);
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
        managed_run.worker_ref = Some(test_worker_ref(u32::MAX));
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

    let summary = state
        .stores
        .run_summaries
        .get(&run_id, Utc::now())
        .await
        .unwrap()
        .unwrap();
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

    let (transport, _control_rx) = worker_transport_with_receiver(run_id).await;
    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.status = RunStatus::Running;
        managed_run.worker_ref = Some(test_worker_ref(u32::MAX));
        managed_run.answer_transport = Some(transport);
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

    seed_lifecycle(&state, run_id, vec![
        run_records::transition(RunLifecycleKind::Starting, RunStatus::Starting),
        run_records::transition(RunLifecycleKind::Running, RunStatus::Running),
        run_records::transition(RunLifecycleKind::Blocked, RunStatus::Blocked {
            blocked_reason: BlockedReason::HumanInputRequired,
        }),
    ])
    .await;

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.status = RunStatus::Blocked {
            blocked_reason: BlockedReason::HumanInputRequired,
        };
        managed_run.worker_ref = Some(test_worker_ref(u32::MAX));
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

    let summary = state
        .stores
        .run_summaries
        .get(&run_id, Utc::now())
        .await
        .unwrap()
        .unwrap();
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

    let (transport, _control_rx) = worker_transport_with_receiver(run_id).await;
    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.status = RunStatus::Paused { prior_block: None };
        managed_run.worker_ref = Some(test_worker_ref(u32::MAX));
        managed_run.answer_transport = Some(transport);
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

    let summary = state
        .stores
        .run_summaries
        .get(&run_id, Utc::now())
        .await
        .unwrap()
        .unwrap();
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

    seed_lifecycle(&state, run_id, vec![
        run_records::transition(RunLifecycleKind::Starting, RunStatus::Starting),
        run_records::transition(RunLifecycleKind::Running, RunStatus::Running),
        RunLifecycleRecord::new(RunLifecycleKind::Paused),
        run_records::transition(RunLifecycleKind::Blocked, RunStatus::Blocked {
            blocked_reason: BlockedReason::HumanInputRequired,
        }),
    ])
    .await;

    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&run_id).expect("run should exist");
        managed_run.status = RunStatus::Paused {
            prior_block: Some(BlockedReason::HumanInputRequired),
        };
        managed_run.worker_ref = Some(test_worker_ref(u32::MAX));
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

    let summary = state
        .stores
        .run_summaries
        .get(&run_id, Utc::now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.lifecycle.status, RunStatus::Blocked {
        blocked_reason: BlockedReason::HumanInputRequired,
    });
    assert_eq!(summary.lifecycle.pending_control, None);
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

#[test]
fn scheduler_capacity_counts_only_runs_occupying_slots() {
    assert!(!counts_toward_scheduler_capacity(RunStatus::Submitted));
    assert!(!counts_toward_scheduler_capacity(RunStatus::Pending {
        reason: PendingReason::ApprovalRequired,
    }));
    assert!(!counts_toward_scheduler_capacity(RunStatus::Runnable));
    assert!(counts_toward_scheduler_capacity(RunStatus::Starting));
    assert!(counts_toward_scheduler_capacity(RunStatus::Running));
    assert!(counts_toward_scheduler_capacity(RunStatus::Blocked {
        blocked_reason: BlockedReason::HumanInputRequired,
    }));
    assert!(counts_toward_scheduler_capacity(RunStatus::Paused {
        prior_block: None,
    }));
    assert!(!counts_toward_scheduler_capacity(RunStatus::Removing));
    assert!(!counts_toward_scheduler_capacity(RunStatus::Succeeded {
        reason: SuccessReason::Completed,
    }));
    assert!(!counts_toward_scheduler_capacity(RunStatus::Failed {
        reason: FailureReason::WorkflowError,
    }));
    assert!(!counts_toward_scheduler_capacity(RunStatus::Dead));
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
        .body(intent_body(&app, MINIMAL_DOT).await)
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
async fn create_completion_invalid_reasoning_effort_returns_422() {
    let app = test_app_with();

    let req = Request::builder()
        .method("POST")
        .uri(api("/completions"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "messages": [],
                "reasoning_effort": "bogus"
            })
            .to_string(),
        ))
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
                        "content": [{"type": "text", "text": "hi"}]
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
        "unknown model provider 'missing-provider'"
    );
}

#[tokio::test]
async fn create_completion_unsupported_reasoning_efforts_return_bad_request() {
    let upstream = MockServer::start();
    let completion = upstream.mock(|when, then| {
        when.method(POST);
        then.status(500);
    });
    let state = TestAppStateBuilder::new()
        .provider_base_url("moonshot", upstream.url("/v1"))
        .vault_entries([(EnvVars::KIMI_API_KEY, "test-kimi-api-key")])
        .build();
    let app = crate::test_support::build_test_router(state);

    for stream in [false, true] {
        for effort in ["medium", "xhigh"] {
            let req = Request::builder()
                .method("POST")
                .uri(api("/completions"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "provider": "moonshot",
                        "model": "kimi-k3",
                        "reasoning_effort": effort,
                        "stream": stream,
                        "messages": [
                            {
                                "role": "user",
                                "content": [{"type": "text", "text": "hi"}]
                            }
                        ]
                    })
                    .to_string(),
                ))
                .unwrap();

            let response = app.clone().oneshot(req).await.unwrap();
            let body = response_json!(response, StatusCode::BAD_REQUEST).await;
            assert_eq!(
                body["errors"][0]["detail"], "model moonshot/kimi-k3 does not support reasoning",
                "stream={stream} effort={effort}"
            );
        }
    }

    completion.assert_calls(0);
}

#[tokio::test]
async fn create_completion_returns_disjoint_usage_buckets() {
    let upstream = MockServer::start();
    let completion = upstream.mock(|when, then| {
        when.method(POST).path("/v1/chat/completions");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "id": "chatcmpl-usage",
                "model": "kimi-k3",
                "choices": [{
                    "message": {"role": "assistant", "content": "OK"},
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 200,
                    "completion_tokens": 30,
                    "total_tokens": 230,
                    "prompt_tokens_details": {
                        "cached_tokens": 50,
                        "cache_write_tokens": 100
                    },
                    "completion_tokens_details": {
                        "reasoning_tokens": 20
                    }
                }
            }));
    });
    let state = TestAppStateBuilder::new()
        .provider_base_url("moonshot", upstream.base_url())
        .vault_entries([(EnvVars::KIMI_API_KEY, "test-kimi-api-key")])
        .build();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/completions"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "provider": "moonshot",
                "model": "kimi-k3",
                "stream": false,
                "messages": [{
                    "role": "user",
                    "content": [{"type": "text", "text": "hi"}]
                }]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(
        body["usage"],
        json!({
            "input": 50,
            "output": 10,
            "reasoning": 20,
            "cache_read": 50,
            "cache_write": 100
        })
    );
    completion.assert();
}

#[tokio::test]
async fn create_completion_default_model_uses_app_state_catalog() {
    let upstream = MockServer::start();
    let completion = upstream.mock(|when, then| {
        when.method(POST)
            .path("/v1/chat/completions")
            .json_body_includes(r#"{"model":"acme-large"}"#);
        then.status(500)
            .header("content-type", "application/json")
            .json_body(json!({"error": {"message": "expected test failure"}}));
    });
    let overlay = acme_overlay(&upstream.base_url());
    let state = TestAppStateBuilder::new()
        .llm_overlay_toml(&overlay)
        .vault_entries([("ACME_API_KEY", "acme-test-key")])
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
                        "content": [{"type": "text", "text": "hi"}]
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
            .contains("expected test failure"),
        "unexpected error body: {body:?}"
    );
    assert!(completion.calls() >= 1);
}

#[tokio::test]
async fn create_completion_structured_output_forwards_reasoning_effort() {
    let upstream = MockServer::start();
    let completion = upstream.mock(|when, then| {
        when.method(POST)
            .path("/v1/chat/completions")
            .json_body_includes(r#"{"model":"kimi-k3","reasoning_effort":"high"}"#);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "id": "chatcmpl-kimi-structured",
                "model": "kimi-k3",
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "{\"answer\":42}"
                    },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 4,
                    "total_tokens": 14
                }
            }));
    });
    let state = TestAppStateBuilder::new()
        .provider_base_url("moonshot", upstream.base_url())
        .vault_entries([(EnvVars::KIMI_API_KEY, "test-kimi-api-key")])
        .build();
    let app = crate::test_support::build_test_router(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/completions"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "provider": "moonshot",
                "model": "kimi-k3",
                "reasoning_effort": "high",
                "stream": false,
                "schema": {
                    "type": "object",
                    "properties": {
                        "answer": {"type": "integer"}
                    },
                    "required": ["answer"]
                },
                "messages": [
                    {
                        "role": "user",
                        "content": [{"type": "text", "text": "Return the answer."}]
                    }
                ]
            })
            .to_string(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["output"], json!({"answer": 42}));
    completion.assert();
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
    assert_eq!(item["usage"]["tokens"]["input"], 0);
    assert!(item["usage"].get("cost").is_none());
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

#[tokio::test]
async fn workflow_version_registration_requires_user_or_run_tools_capability() {
    let (state, app) = jwt_auth_app();
    let run_id = RunId::new();
    let body = json!({
        "entrypoint": "workflow.fabro",
        "files": {"workflow.fabro": "digraph W {}"},
        "workflow_dependencies": {},
    });
    for (token, expected) in [
        (issue_test_user_jwt(), StatusCode::CREATED),
        (
            issue_test_run_tools_worker_token(&run_id),
            StatusCode::CREATED,
        ),
        (issue_test_worker_token(&run_id), StatusCode::FORBIDDEN),
    ] {
        let response = app
            .clone()
            .oneshot(json_bearer_request(
                Method::POST,
                "/workflow-versions",
                &token,
                &body,
            ))
            .await
            .unwrap();
        fabro_test::expect_axum_status(response, expected, "POST /workflow-versions actor matrix")
            .await;
    }
    for body in [serde_json::to_string(&body).unwrap(), "{".to_string()] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(api("/workflow-versions"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        fabro_test::expect_axum_status(
            response,
            StatusCode::UNAUTHORIZED,
            "anonymous POST /workflow-versions",
        )
        .await;
    }
    let response = app
        .oneshot(bearer_request(
            Method::GET,
            "/runs",
            &issue_test_user_jwt(),
            Body::empty(),
        ))
        .await
        .unwrap();
    let listed =
        fabro_test::expect_axum_json(response, StatusCode::OK, "GET /runs after registration")
            .await;
    assert_eq!(listed["data"], json!([]));
    assert!(
        state
            .stores
            .runs
            .load_run_projection(&run_id)
            .await
            .unwrap()
            .is_none()
    );
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
    let db_pool = test_db_pool_for_vault_path(&vault_path).expect("test db pool should build");
    let preloaded_vault = crate::test_support::test_secret_snapshot(db_pool.clone())
        .expect("test secret snapshot should build");
    let Err(err) = build_app_state(AppStateConfig {
        subprocess_executable: None,
        resolved_settings: resolved_runtime_settings_for_tests(
            server_settings,
            RunLayer::default(),
            LlmLayer::default(),
        ),
        execute_in_process: false,
        max_concurrent_runs: 5,
        store,
        artifact_store,
        db_pool,
        preloaded_vault,
        server_secrets: ServerSecrets::load(server_env_path, HashMap::new()).unwrap(),
        env_lookup: default_env_lookup(),
        github_api_base_url: None,
        active_config_path: tempfile::tempdir().unwrap().path().join("settings.toml"),
        http_client: Some(fabro_http::test_http_client().expect("test HTTP client should build")),
        sandbox_inventory: None,
        shutdown: tokio_util::sync::CancellationToken::new(),
        worker_control_bus: None,
        worker_runtime: None,
        automation_materializer_override: None,
    }) else {
        panic!("build_app_state should require SESSION_SECRET")
    };

    assert!(err.to_string().contains(
        "Fabro server refuses to start: auth is configured but SESSION_SECRET is not set."
    ));
}

#[test]
fn slack_service_respects_disabled_server_config_even_with_vault_tokens() {
    let mut settings = default_test_server_settings();
    settings.server.integrations.slack.enabled = false;
    let (store, artifact_store) = test_store_bundle();
    let vault_path = test_secret_store_path();
    let mut vault = Vault::load(vault_path.clone()).unwrap();
    vault
        .set(
            EnvVars::FABRO_SLACK_BOT_TOKEN,
            "xoxb-test",
            SecretType::Token,
            None,
        )
        .unwrap();
    vault
        .set(
            EnvVars::FABRO_SLACK_APP_TOKEN,
            "xapp-test",
            SecretType::Token,
            None,
        )
        .unwrap();

    let state = build_app_state(AppStateConfig {
        subprocess_executable: None,
        resolved_settings: resolved_runtime_settings_for_tests(
            settings,
            RunLayer::default(),
            LlmLayer::default(),
        ),
        execute_in_process: false,
        max_concurrent_runs: 5,
        store,
        artifact_store,
        db_pool: test_db_pool_for_vault_path(&vault_path).expect("test db pool should build"),
        preloaded_vault: vault,
        server_secrets: load_test_server_secrets(
            tempfile::tempdir().unwrap().path().join("server.env"),
            HashMap::new(),
        ),
        env_lookup: default_env_lookup(),
        github_api_base_url: None,
        active_config_path: tempfile::tempdir().unwrap().path().join("settings.toml"),
        http_client: Some(fabro_http::test_http_client().expect("test HTTP client should build")),
        sandbox_inventory: None,
        shutdown: tokio_util::sync::CancellationToken::new(),
        worker_control_bus: None,
        worker_runtime: None,
        automation_materializer_override: None,
    })
    .expect("slack disabled test app state should build");

    assert!(state.slack_service.is_none());
}

#[tokio::test]
async fn run_tools_worker_registers_contents_then_creates_by_version_id() {
    let (state, app) = jwt_auth_app();
    let parent_id = create_run_with_bearer(&app, &issue_test_user_jwt()).await;
    let token = issue_test_run_tools_worker_token(&parent_id);
    let response = app
        .clone()
        .oneshot(json_bearer_request(
            Method::POST,
            "/workflow-versions",
            &token,
            &json!({
                "entrypoint": "child.fabro",
                "files": {"child.fabro": MINIMAL_DOT},
                "workflow_dependencies": {}
            }),
        ))
        .await
        .unwrap();
    let registered = response_json!(response, StatusCode::CREATED).await;
    let response = app
        .oneshot(json_bearer_request(
            Method::POST,
            "/runs",
            &token,
            &json!({
                "workflow_version_id": registered["workflow_version_id"],
                "target": {"kind": "none"},
                "args": {"dry_run": true},
                "parent_id": parent_id,
                "goal": "A child created from sandbox-supplied contents"
            }),
        ))
        .await
        .unwrap();
    let child = response_json!(response, StatusCode::CREATED).await;
    assert_eq!(child["parent_id"], parent_id.to_string());
    assert_eq!(child["lifecycle"]["status"]["kind"], "submitted");
    let child_id = child["id"].as_str().unwrap().parse::<RunId>().unwrap();
    let projection = state
        .stores
        .runs
        .load_run_projection(&child_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(projection.spec.workflow_version_id).unwrap(),
        registered["workflow_version_id"]
    );
    assert_eq!(projection.spec.target, Some(RunTarget::None {}));
    assert!(projection.start.is_none());
}

/// A run settled at its terminal record keeps that status at its worker's
/// exit, whatever the exit status, unless the store ended the run
/// differently: then the store's terminal status stands.
#[test]
fn a_settled_run_keeps_its_status_at_worker_exit() {
    let settled = RunStatus::Succeeded {
        reason: SuccessReason::Completed,
    };
    assert_eq!(status_after_worker_exit(settled, settled, false), settled);
    assert_eq!(status_after_worker_exit(settled, settled, true), settled);
    assert_eq!(
        status_after_worker_exit(settled, RunStatus::Running, false),
        settled
    );
    let recorded = RunStatus::Failed {
        reason: FailureReason::Terminated,
    };
    assert_eq!(status_after_worker_exit(settled, recorded, false), recorded);
}

/// A run its worker left unsettled takes the store's final status; with
/// none recorded, an unsuccessful exit is a termination and a successful
/// one changes nothing.
#[test]
fn an_unsettled_run_takes_the_stores_status_or_a_termination_at_worker_exit() {
    let failed = RunStatus::Failed {
        reason: FailureReason::WorkflowError,
    };
    assert_eq!(
        status_after_worker_exit(RunStatus::Running, failed, false),
        failed
    );
    assert_eq!(
        status_after_worker_exit(RunStatus::Running, RunStatus::Running, false),
        RunStatus::Failed {
            reason: FailureReason::Terminated,
        }
    );
    assert_eq!(
        status_after_worker_exit(RunStatus::Running, RunStatus::Running, true),
        RunStatus::Running
    );
}
