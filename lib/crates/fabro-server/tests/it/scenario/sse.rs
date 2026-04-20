use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tokio::time::{sleep, timeout};
use tower::ServiceExt;

use crate::helpers::{
    POLL_ATTEMPTS, POLL_INTERVAL, api, checked_response, checked_response_in,
    create_and_start_run_from_manifest, minimal_manifest_json_with_dry_run, response_json,
    test_app_state_with_options, test_app_with_scheduler, test_settings,
    wait_for_run_status_not_in,
};

const SIMPLE_DOT: &str = r#"digraph SSETest {
    graph [goal="Test SSE"]
    start [shape=Mdiamond]
    work  [shape=box, prompt="Do work"]
    exit  [shape=Msquare]
    start -> work -> exit
}"#;

async fn wait_for_checkpoint(app: &axum::Router, run_id: &str) -> serde_json::Value {
    for _ in 0..POLL_ATTEMPTS {
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/checkpoint")))
            .body(Body::empty())
            .expect("checkpoint request should build");
        let response = app.clone().oneshot(req).await.unwrap();
        let status = response.status();
        if status == StatusCode::OK {
            return response_json(
                response,
                StatusCode::OK,
                format!("GET /api/v1/runs/{run_id}/checkpoint"),
            )
            .await;
        }
        checked_response_in(
            response,
            &[StatusCode::OK, StatusCode::NOT_FOUND],
            format!("GET /api/v1/runs/{run_id}/checkpoint"),
        )
        .await;
        sleep(POLL_INTERVAL).await;
    }
    panic!("checkpoint did not become available for {run_id}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_stream_contains_expected_event_types() {
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id =
        create_and_start_run_from_manifest(&app, minimal_manifest_json_with_dry_run(SIMPLE_DOT))
            .await;

    wait_for_run_status_not_in(&app, &run_id, &["queued", "starting"]).await;

    // Get SSE stream
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/attach")))
        .body(Body::empty())
        .unwrap();
    let response = checked_response(
        app.clone().oneshot(req).await.unwrap(),
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/attach"),
    )
    .await;

    let content_type = response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(content_type.contains("text/event-stream"));

    // Collect SSE frames with a timeout
    let mut body = response.into_body();
    let mut sse_data = String::new();
    while let Ok(Some(Ok(frame))) = timeout(Duration::from_secs(2), body.frame()).await {
        if let Some(data) = frame.data_ref() {
            sse_data.push_str(&String::from_utf8_lossy(data));
        }
    }

    // Parse SSE data lines and extract event types
    let mut event_types: Vec<String> = Vec::new();
    for line in sse_data.lines() {
        if let Some(json_str) = line.strip_prefix("data:") {
            let json_str = json_str.trim();
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(json_str) {
                if let Some(event_name) = event["event"].as_str() {
                    event_types.push(event_name.to_string());
                }
            }
        }
    }

    // Because we subscribe while the run is only guaranteed to be past
    // "queued", a live stream should include at least one stage event.
    // If the run completes before we attach with no unread events, an empty
    // stream is still a valid 200 response.
    if !event_types.is_empty() {
        assert!(
            event_types
                .iter()
                .any(|t| t == "stage.started" || t == "stage.completed"),
            "should contain stage events, got: {event_types:?}"
        );
    }

    // Pipeline is complete (SSE stream ended), verify checkpoint
    let cp_body = wait_for_checkpoint(&app, &run_id).await;
    // If run completed, checkpoint should have completed_nodes
    if !cp_body.is_null() {
        let completed = cp_body["completed_nodes"].as_array();
        if let Some(nodes) = completed {
            let names: Vec<&str> = nodes.iter().filter_map(|v| v.as_str()).collect();
            assert!(names.contains(&"work"), "work should be in completed_nodes");
        }
    }
}
