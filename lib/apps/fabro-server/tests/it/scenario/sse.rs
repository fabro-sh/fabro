use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tokio::time::timeout;
use tower::ServiceExt;

use crate::helpers::{
    api, checked_response, create_and_start_run_from_intent, minimal_intent_json_with_dry_run,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_stream_contains_expected_event_types() {
    let workspace = tempfile::tempdir().unwrap();
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id = create_and_start_run_from_intent(
        &app,
        minimal_intent_json_with_dry_run(&app, SIMPLE_DOT, workspace.path()).await,
    )
    .await;

    wait_for_run_status_not_in(&app, &run_id, &["runnable", "starting"]).await;

    // Replay from the beginning so the assertion does not depend on whether
    // the run advances before the attach request is handled.
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/attach?after=0")))
        .body(Body::empty())
        .unwrap();
    let response = checked_response(
        app.clone().oneshot(req).await.unwrap(),
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/attach?after=0"),
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

    // Every frame is one stream item: Petri's events name the stage they
    // belong to, so the run's stages show up as `visit.started`.
    let mut event_types: Vec<String> = Vec::new();
    for line in sse_data.lines() {
        if let Some(json_str) = line.strip_prefix("data:") {
            if let Ok(item) = serde_json::from_str::<serde_json::Value>(json_str.trim()) {
                let name = item["item"]["record"]["body"]["event"]
                    .as_str()
                    .or_else(|| item["item"]["derived"]["event"].as_str());
                if let Some(name) = name {
                    event_types.push(name.to_string());
                }
            }
        }
    }

    assert!(
        event_types.iter().any(|t| t == "visit.started"),
        "should contain stage events, got: {event_types:?}"
    );
}
