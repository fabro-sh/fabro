use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_config::Storage;
use fabro_server::server::create_app_state_with_options;
use fabro_types::{RunId, Settings};
use http_body_util::BodyExt;
use tempfile::tempdir;
use tokio::time::timeout;
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, body_json, minimal_manifest_json_with_dry_run, test_app_with_scheduler,
    test_settings, wait_for_run_status,
};

fn temp_storage_settings() -> (tempfile::TempDir, Settings) {
    let temp = tempdir().expect("tempdir should create");
    let mut settings = test_settings();
    settings.dry_run = Some(true);
    settings.storage_dir = Some(temp.path().join("storage"));
    (temp, settings)
}

async fn create_run(app: &axum::Router, manifest: serde_json::Value) -> String {
    let request = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&manifest).unwrap()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let body = body_json(response.into_body()).await;
    body["id"].as_str().unwrap().to_string()
}

async fn start_run(app: &axum::Router, run_id: &str) {
    let request = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/start")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn get_system_info_returns_runtime_fields() {
    let (_temp, settings) = temp_storage_settings();
    let expected_storage_dir = settings.storage_dir.clone().unwrap();
    let app = fabro_server::server::build_router(
        create_app_state_with_options(settings, 5),
        fabro_server::jwt_auth::AuthMode::Disabled,
    );

    let request = Request::builder()
        .method("GET")
        .uri(api("/system/info"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response.into_body()).await;
    assert!(body["version"].as_str().is_some());
    assert_eq!(body["storage_engine"], "slatedb");
    assert_eq!(
        body["storage_dir"],
        expected_storage_dir.display().to_string()
    );
    assert_eq!(body["runs"]["total"], 0);
    assert_eq!(body["runs"]["active"], 0);
    assert!(body["uptime_secs"].as_i64().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_system_disk_usage_returns_summary_and_verbose_rows() {
    let (_temp, settings) = temp_storage_settings();
    let storage_dir = settings.storage_dir.clone().unwrap();
    let app = test_app_with_scheduler(create_app_state_with_options(settings, 5));

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

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response.into_body()).await;
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
    let (_temp, settings) = temp_storage_settings();
    let storage_dir = settings.storage_dir.clone().unwrap();
    let app = test_app_with_scheduler(create_app_state_with_options(settings, 5));

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
    assert_eq!(dry_run_response.status(), StatusCode::OK);
    let dry_run_body = body_json(dry_run_response.into_body()).await;
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
    assert_eq!(delete_response.status(), StatusCode::OK);
    let delete_body = body_json(delete_response.into_body()).await;
    assert_eq!(delete_body["dry_run"], false);
    assert_eq!(delete_body["deleted_count"], 1);
    assert!(!run_dir.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_events_streams_only_matching_run_ids() {
    let (_temp, settings) = temp_storage_settings();
    let app = test_app_with_scheduler(create_app_state_with_options(settings, 5));

    let run_one = create_run(&app, minimal_manifest_json_with_dry_run(MINIMAL_DOT)).await;
    let run_two = create_run(&app, minimal_manifest_json_with_dry_run(MINIMAL_DOT)).await;

    let request = Request::builder()
        .method("GET")
        .uri(api(&format!("/attach?run_id={run_one}")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .expect("content-type should be present")
        .to_str()
        .unwrap();
    assert!(content_type.contains("text/event-stream"));

    start_run(&app, &run_one).await;
    start_run(&app, &run_two).await;

    let mut body = response.into_body();
    let mut sse_data = String::new();
    while let Ok(Some(Ok(frame))) = timeout(Duration::from_secs(2), body.frame()).await {
        if let Some(data) = frame.data_ref() {
            sse_data.push_str(&String::from_utf8_lossy(data));
            if sse_data.contains(&run_one) {
                break;
            }
        }
    }

    assert!(
        sse_data.contains(&run_one),
        "expected filtered stream data: {sse_data}"
    );
    assert!(
        !sse_data.contains(&run_two),
        "filtered stream should exclude non-matching run ids: {sse_data}"
    );
}
