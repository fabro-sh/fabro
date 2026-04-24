use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use fabro_config::{LocalSandboxLayer, RunLayer, RunSandboxLayer, ServerSettingsBuilder};
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::{
    AppState, build_router, create_app_state,
    create_app_state_with_runtime_settings_and_env_lookup,
    create_app_state_with_runtime_settings_and_options_and_registry_factory, spawn_scheduler,
};
use fabro_test::{
    assert_axum_status, assert_reqwest_status, expect_axum_json, expect_axum_status,
    expect_axum_status_in, expect_axum_text,
};
use fabro_types::ServerSettings;
use fabro_types::settings::run::WorktreeMode;
use tokio::time::sleep;
use tower::ServiceExt;

pub(crate) const MINIMAL_DOT: &str = r#"digraph Test {
    graph [goal="Test"]
    start [shape=Mdiamond]
    exit  [shape=Msquare]
    start -> exit
}"#;

pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(10);
pub(crate) const POLL_ATTEMPTS: usize = 500;

#[derive(Clone)]
pub(crate) struct TestAppSettings {
    pub server_settings:       ServerSettings,
    pub manifest_run_defaults: RunLayer,
}

impl Default for TestAppSettings {
    fn default() -> Self {
        settings_from_toml("_version = 1\n")
    }
}

fn ensure_test_auth_methods(document: &mut toml::Table) {
    let server = document
        .entry("server")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .expect("[server] should stay a table in test fixtures");
    let auth = server
        .entry("auth")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .expect("[server.auth] should stay a table in test fixtures");
    auth.entry("methods")
        .or_insert_with(|| toml::Value::Array(vec![toml::Value::String("dev-token".to_string())]));
}

pub(crate) fn settings_from_toml(source: &str) -> TestAppSettings {
    let mut document: toml::Table = source.parse().expect("test fixture should parse as TOML");
    ensure_test_auth_methods(&mut document);
    let manifest_run_defaults = document
        .remove("run")
        .map(toml::Value::try_into::<RunLayer>)
        .transpose()
        .expect("test run settings should parse")
        .unwrap_or_default();
    let server_settings = ServerSettingsBuilder::from_toml(
        &toml::to_string(&document).expect("test fixture should serialize"),
    )
    .expect("test server settings should resolve");
    TestAppSettings {
        server_settings,
        manifest_run_defaults,
    }
}

pub(crate) fn test_app_state() -> Arc<AppState> {
    create_app_state()
}

pub(crate) fn test_app_state_with_options(
    settings: TestAppSettings,
    max_concurrent_runs: usize,
) -> Arc<AppState> {
    create_app_state_with_runtime_settings_and_options_and_registry_factory(
        settings.server_settings,
        settings.manifest_run_defaults,
        max_concurrent_runs,
        |interviewer| fabro_workflow::handler::default_registry(interviewer, || None),
    )
}

pub(crate) fn test_settings() -> TestAppSettings {
    TestAppSettings {
        manifest_run_defaults: RunLayer {
            sandbox: Some(RunSandboxLayer {
                local: Some(LocalSandboxLayer {
                    worktree_mode: Some(WorktreeMode::Never),
                }),
                ..RunSandboxLayer::default()
            }),
            ..RunLayer::default()
        },
        ..TestAppSettings::default()
    }
}

pub(crate) fn test_app_with_scheduler(state: Arc<AppState>) -> axum::Router {
    spawn_scheduler(Arc::clone(&state));
    build_router(state, AuthMode::Disabled)
}

pub(crate) fn test_app_with_no_providers() -> axum::Router {
    let settings = test_settings();
    let state = create_app_state_with_runtime_settings_and_env_lookup(
        settings.server_settings,
        settings.manifest_run_defaults,
        5,
        |_| None,
    );
    build_router(state, AuthMode::Disabled)
}

pub(crate) fn test_app_with_mock_anthropic(mock_base_url: &str) -> axum::Router {
    let base_url = mock_base_url.to_string();
    let settings = test_settings();
    let state = create_app_state_with_runtime_settings_and_env_lookup(
        settings.server_settings,
        settings.manifest_run_defaults,
        5,
        move |name| match name {
            "ANTHROPIC_API_KEY" => Some("test-key".to_string()),
            "ANTHROPIC_BASE_URL" => Some(base_url.clone()),
            _ => None,
        },
    );
    build_router(state, AuthMode::Disabled)
}

pub(crate) fn api(path: &str) -> String {
    format!("/api/v1{path}")
}

pub(crate) fn repo_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("fabro-server crate should be nested under lib/crates/fabro-server")
        .to_path_buf()
}

#[expect(
    clippy::disallowed_methods,
    reason = "test fixture reads tracked files synchronously"
)]
pub(crate) fn read_repo_file(relative_path: &str) -> String {
    let path = repo_root().join(relative_path);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

pub(crate) async fn body_json(body: Body) -> serde_json::Value {
    let bytes = to_bytes(body, usize::MAX)
        .await
        .expect("response body should fit in memory");
    serde_json::from_slice(&bytes).expect("response body should be valid JSON")
}

pub(crate) async fn response_status(
    response: axum::response::Response,
    expected: StatusCode,
    context: impl std::fmt::Display,
) {
    assert_axum_status(response, expected, context).await;
}

pub(crate) async fn response_json(
    response: axum::response::Response,
    expected: StatusCode,
    context: impl std::fmt::Display,
) -> serde_json::Value {
    expect_axum_json(response, expected, context).await
}

pub(crate) async fn response_text(
    response: axum::response::Response,
    expected: StatusCode,
    context: impl std::fmt::Display,
) -> String {
    expect_axum_text(response, expected, context).await
}

pub(crate) async fn checked_response(
    response: axum::response::Response,
    expected: StatusCode,
    context: impl std::fmt::Display,
) -> axum::response::Response {
    expect_axum_status(response, expected, context).await
}

pub(crate) async fn checked_response_in(
    response: axum::response::Response,
    expected: &[StatusCode],
    context: impl std::fmt::Display,
) -> axum::response::Response {
    expect_axum_status_in(response, expected, context).await
}

pub(crate) async fn reqwest_status(
    response: fabro_http::Response,
    expected: StatusCode,
    context: impl std::fmt::Display,
) {
    assert_reqwest_status(response, expected, context).await;
}

pub(crate) async fn create_and_start_run_from_manifest(
    app: &axum::Router,
    manifest: serde_json::Value,
) -> String {
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&manifest).expect("manifest fixture should serialize"),
        ))
        .expect("create-run request should build");
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json(response, StatusCode::CREATED, "POST /api/v1/runs").await;
    let run_id = body["id"]
        .as_str()
        .expect("create-run response should include an id")
        .to_string();

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/start")))
        .body(Body::empty())
        .expect("start-run request should build");
    response_status(
        app.clone().oneshot(req).await.unwrap(),
        StatusCode::OK,
        format!("POST /api/v1/runs/{run_id}/start"),
    )
    .await;

    run_id
}

pub(crate) fn minimal_manifest_json(dot_source: &str) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "cwd": "/tmp",
        "target": {
            "identifier": "workflow.fabro",
            "path": "workflow.fabro"
        },
        "workflows": {
            "workflow.fabro": {
                "source": dot_source,
                "files": {}
            }
        }
    })
}

pub(crate) fn minimal_manifest_json_with_dry_run(dot_source: &str) -> serde_json::Value {
    let mut manifest = minimal_manifest_json(dot_source);
    manifest["args"] = serde_json::json!({ "dry_run": true });
    manifest
}

pub(crate) async fn run_json(app: &axum::Router, run_id: &str) -> serde_json::Value {
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .expect("run lookup request should build");
    let response = app.clone().oneshot(req).await.unwrap();
    response_json(
        response,
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}"),
    )
    .await
}

pub(crate) async fn wait_for_run_status(
    app: &axum::Router,
    run_id: &str,
    expected: &[&str],
) -> String {
    for _ in 0..POLL_ATTEMPTS {
        let body = run_json(app, run_id).await;
        let status = body["status"]["kind"]
            .as_str()
            .expect("run response should include a tagged status kind")
            .to_string();
        if expected.iter().any(|candidate| *candidate == status) {
            return status;
        }
        sleep(POLL_INTERVAL).await;
    }
    panic!("run {run_id} did not reach any of {expected:?}");
}

pub(crate) async fn wait_for_run_status_not_in(
    app: &axum::Router,
    run_id: &str,
    unexpected: &[&str],
) -> String {
    for _ in 0..POLL_ATTEMPTS {
        let body = run_json(app, run_id).await;
        let status = body["status"]["kind"]
            .as_str()
            .expect("run response should include a tagged status kind")
            .to_string();
        if unexpected.iter().all(|candidate| *candidate != status) {
            return status;
        }
        sleep(POLL_INTERVAL).await;
    }
    panic!("run {run_id} stayed in {unexpected:?}");
}
