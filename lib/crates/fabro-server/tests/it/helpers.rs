use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::{
    AppState, build_router, create_app_state, create_app_state_with_options, spawn_scheduler,
};
use fabro_types::Settings;
use fabro_types::settings::{LocalSandboxSettings, SandboxSettings, WorktreeMode};
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

pub(crate) fn test_app_state() -> Arc<AppState> {
    create_app_state()
}

pub(crate) fn test_settings() -> Settings {
    Settings {
        sandbox: Some(SandboxSettings {
            local: Some(LocalSandboxSettings {
                worktree_mode: WorktreeMode::Never,
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub(crate) fn dry_run_settings() -> Settings {
    Settings {
        dry_run: Some(true),
        ..test_settings()
    }
}

pub(crate) fn dry_run_app() -> axum::Router {
    let state = create_app_state_with_options(dry_run_settings(), 5);
    spawn_scheduler(Arc::clone(&state));
    build_router(state, AuthMode::Disabled)
}

pub(crate) fn test_app_with_scheduler(state: Arc<AppState>) -> axum::Router {
    spawn_scheduler(Arc::clone(&state));
    build_router(state, AuthMode::Disabled)
}

pub(crate) fn api(path: &str) -> String {
    format!("/api/v1{path}")
}

pub(crate) async fn body_json(body: Body) -> serde_json::Value {
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Create a run via POST /runs, then start it via POST /runs/{id}/start.
/// Returns the run_id string.
pub(crate) async fn create_and_start_run(app: &axum::Router, dot_source: &str) -> String {
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({"dot_source": dot_source})).unwrap(),
        ))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap().to_string();

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/start")))
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(req).await.unwrap();

    run_id
}

pub(crate) async fn run_json(app: &axum::Router, run_id: &str) -> serde_json::Value {
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response.into_body()).await
}

pub(crate) async fn wait_for_run_status(
    app: &axum::Router,
    run_id: &str,
    expected: &[&str],
) -> String {
    for _ in 0..POLL_ATTEMPTS {
        let body = run_json(app, run_id).await;
        let status = body["status"].as_str().unwrap().to_string();
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
        let status = body["status"].as_str().unwrap().to_string();
        if unexpected.iter().all(|candidate| *candidate != status) {
            return status;
        }
        sleep(POLL_INTERVAL).await;
    }
    panic!("run {run_id} stayed in {unexpected:?}");
}
