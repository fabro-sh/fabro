use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_server::server::create_app_state_with_options;
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::helpers::{
    POLL_ATTEMPTS, POLL_INTERVAL, api, body_json, create_and_start_run, dry_run_settings,
    test_app_with_scheduler, wait_for_run_status_not_in,
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
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        if response.status() == StatusCode::OK {
            return body_json(response.into_body()).await;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("checkpoint did not become available for {run_id}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_stream_contains_expected_event_types() {
    let state = create_app_state_with_options(dry_run_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id = create_and_start_run(&app, SIMPLE_DOT).await;

    wait_for_run_status_not_in(&app, &run_id, &["queued", "starting"]).await;

    // Get SSE stream
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/attach")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    // May be 200 (stream open) or 410 (run completed before connect)
    let sse_status = response.status();
    assert!(
        sse_status == StatusCode::OK || sse_status == StatusCode::GONE,
        "expected 200 or 410, got: {sse_status}"
    );
    if sse_status == StatusCode::GONE {
        return;
    }

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
    while let Ok(Some(Ok(frame))) = tokio::time::timeout(Duration::from_secs(2), body.frame()).await
    {
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
                if let Some(event_name) = event["payload"]["event"].as_str() {
                    event_types.push(event_name.to_string());
                }
            }
        }
    }

    // Because we subscribe while the run is only guaranteed to be past
    // "queued", a live stream should include at least one stage event.
    // A 410 response above still covers the case where the run completed
    // before we managed to attach.
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
