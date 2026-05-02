#![expect(
    clippy::disallowed_methods,
    reason = "integration tests stage fixtures with sync std::fs; test infrastructure, not Tokio-hot path"
)]

use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_config::Storage;
use fabro_types::RunId;
use fabro_types::settings::interp::InterpString;
use tempfile::tempdir;
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, POLL_ATTEMPTS, POLL_INTERVAL, TestAppSettings, api, checked_response,
    minimal_manifest_json, minimal_manifest_json_with_dry_run, response_json, response_status,
    test_app_state_with_options, test_app_with_scheduler, test_settings, wait_for_run_status,
};

const HUMAN_GATE_DOT: &str = r#"digraph GateTest {
    graph [goal="Test gate"]
    start [shape=Mdiamond]
    exit  [shape=Msquare]
    work  [shape=box, prompt="Do work"]
    gate  [shape=hexagon, type="human", label="Approve?"]
    done  [shape=box, prompt="Finish"]
    revise [shape=box, prompt="Revise"]

    start -> work -> gate
    gate -> done   [label="[A] Approve"]
    gate -> revise [label="[R] Revise"]
    done -> exit
    revise -> gate
}"#;

fn temp_storage_settings() -> (tempfile::TempDir, TestAppSettings, PathBuf) {
    let temp = tempdir().expect("tempdir should create");
    let mut settings = test_settings();
    let storage_dir = temp.path().join("storage");
    settings.server_settings.server.storage.root =
        InterpString::parse(&storage_dir.to_string_lossy());
    (temp, settings, storage_dir)
}

async fn create_run(app: &axum::Router, manifest: serde_json::Value) -> String {
    let request = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&manifest).expect("manifest fixture should serialize"),
        ))
        .expect("create-run request should build");
    let response = app.clone().oneshot(request).await.unwrap();
    let body = response_json(response, StatusCode::CREATED, "POST /api/v1/runs").await;
    body["id"]
        .as_str()
        .expect("create-run response should include an id")
        .to_string()
}

async fn start_run(app: &axum::Router, run_id: &str) {
    let request = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/start")))
        .body(Body::empty())
        .expect("start-run request should build");
    let response = app.clone().oneshot(request).await.unwrap();
    response_status(
        response,
        StatusCode::OK,
        format!("POST /api/v1/runs/{run_id}/start"),
    )
    .await;
}

async fn wait_for_question(app: &axum::Router, run_id: &str) -> serde_json::Value {
    for _ in 0..POLL_ATTEMPTS {
        let request = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/questions")))
            .body(Body::empty())
            .expect("questions request should build");
        let response = app.clone().oneshot(request).await.unwrap();
        let body = response_json(
            response,
            StatusCode::OK,
            format!("GET /api/v1/runs/{run_id}/questions"),
        )
        .await;
        if let Some(question) = body["data"].as_array().and_then(|items| items.first()) {
            return question.clone();
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("question should have appeared for {run_id}");
}

async fn load_questions(app: &axum::Router, run_id: &str) -> serde_json::Value {
    let request = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/questions")))
        .body(Body::empty())
        .expect("questions request should build");
    let response = app.clone().oneshot(request).await.unwrap();
    response_json(
        response,
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/questions"),
    )
    .await
}

#[tokio::test]
async fn get_system_info_returns_runtime_fields() {
    let (_temp, settings, expected_storage_dir) = temp_storage_settings();
    let configured_server_url = settings.server_settings.server.web.url.as_source();
    let app =
        fabro_server::test_support::build_test_router(test_app_state_with_options(settings, 5));

    let request = Request::builder()
        .method("GET")
        .uri(api("/system/info"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();

    let body = response_json(response, StatusCode::OK, "GET /api/v1/system/info").await;
    assert!(body["version"].as_str().is_some());
    assert_eq!(body["server_url"], configured_server_url);
    assert_eq!(body["storage_engine"], "slatedb");
    assert_eq!(
        body["storage_dir"],
        expected_storage_dir.display().to_string()
    );
    assert_eq!(body["runs"]["total"], 0);
    assert_eq!(body["runs"]["active"], 0);
    assert!(body["uptime_secs"].as_i64().is_some());
    assert_eq!(body["features"]["session_sandboxes"], false);
    assert_eq!(body["features"]["retros"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_app_state_with_options_respects_max_concurrent_runs() {
    let app = test_app_with_scheduler(test_app_state_with_options(test_settings(), 1));

    let first_run = create_run(&app, minimal_manifest_json(HUMAN_GATE_DOT)).await;
    let second_run = create_run(&app, minimal_manifest_json(HUMAN_GATE_DOT)).await;

    start_run(&app, &first_run).await;
    start_run(&app, &second_run).await;

    let question = wait_for_question(&app, &first_run).await;
    assert_eq!(question["stage"], "gate");

    tokio::time::sleep(POLL_INTERVAL * 5).await;

    let second_questions = load_questions(&app, &second_run).await;
    assert!(
        second_questions["data"]
            .as_array()
            .is_some_and(std::vec::Vec::is_empty),
        "second run should still be queued while the first waits at the human gate: {second_questions}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_system_disk_usage_returns_summary_and_verbose_rows() {
    let (_temp, settings, storage_dir) = temp_storage_settings();
    let app = test_app_with_scheduler(test_app_state_with_options(settings, 5));

    let run_id = create_run(&app, minimal_manifest_json_with_dry_run(MINIMAL_DOT)).await;
    start_run(&app, &run_id).await;
    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(status, "succeeded");

    let logs_dir = storage_dir.join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();
    std::fs::write(logs_dir.join("server.log"), b"log line\n").unwrap();

    let request = Request::builder()
        .method("GET")
        .uri(api("/system/df?verbose=true"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();

    let body = response_json(
        response,
        StatusCode::OK,
        "GET /api/v1/system/df?verbose=true",
    )
    .await;
    assert!(body["summary"].is_array());
    assert!(body["total_size_bytes"].as_i64().unwrap_or_default() > 0);
    assert!(
        body["runs"]
            .as_array()
            .is_some_and(|runs| runs.iter().any(|entry| entry["run_id"] == run_id))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prune_runs_supports_dry_run_and_deletion() {
    let (_temp, settings, storage_dir) = temp_storage_settings();
    let app = test_app_with_scheduler(test_app_state_with_options(settings, 5));

    let run_id = create_run(&app, minimal_manifest_json_with_dry_run(MINIMAL_DOT)).await;
    start_run(&app, &run_id).await;
    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(status, "succeeded");

    let run_id_parsed: RunId = run_id.parse().unwrap();
    let run_dir = Storage::new(&storage_dir)
        .run_scratch(&run_id_parsed)
        .root()
        .to_path_buf();
    assert!(run_dir.exists());

    let dry_run_request = Request::builder()
        .method("POST")
        .uri(api("/system/prune/runs"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"before":"9999"}"#))
        .unwrap();
    let dry_run_response = app.clone().oneshot(dry_run_request).await.unwrap();
    let dry_run_body = response_json(
        dry_run_response,
        StatusCode::OK,
        "POST /api/v1/system/prune/runs",
    )
    .await;
    assert_eq!(dry_run_body["dry_run"], true);
    assert_eq!(dry_run_body["total_count"], 1);
    assert_eq!(dry_run_body["runs"][0]["run_id"], run_id);
    assert!(run_dir.exists());

    let delete_request = Request::builder()
        .method("POST")
        .uri(api("/system/prune/runs"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"dry_run":false,"before":"9999"}"#))
        .unwrap();
    let delete_response = app.clone().oneshot(delete_request).await.unwrap();
    let delete_body = response_json(
        delete_response,
        StatusCode::OK,
        "POST /api/v1/system/prune/runs",
    )
    .await;
    assert_eq!(delete_body["dry_run"], false);
    assert_eq!(delete_body["deleted_count"], 1);
    assert!(!run_dir.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_events_returns_sse_stream() {
    let (_temp, settings, _storage_dir) = temp_storage_settings();
    let app = test_app_with_scheduler(test_app_state_with_options(settings, 5));
    let run_id = RunId::new();

    let request = Request::builder()
        .method("GET")
        .uri(api(&format!("/attach?run_id={run_id}")))
        .body(Body::empty())
        .unwrap();
    let response = checked_response(
        app.clone().oneshot(request).await.unwrap(),
        StatusCode::OK,
        format!("GET /api/v1/attach?run_id={run_id}"),
    )
    .await;
    let content_type = response
        .headers()
        .get("content-type")
        .expect("content-type should be present")
        .to_str()
        .unwrap();
    assert!(content_type.contains("text/event-stream"));
}
