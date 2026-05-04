//! Cursor-pagination tests for the per-stage events endpoint (demo mode).
//!
//! The stage-events route uses `since_seq=` + `limit=` (cursor-based) instead
//! of the offset-based `page[limit]/page[offset]` pagination used by other
//! list endpoints, so it gets its own test rather than living in the generic
//! offset-shape matrix.

#![allow(
    clippy::absolute_paths,
    reason = "This test module prefers explicit type paths over extra imports."
)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use super::helpers::{response_json, test_app_state};

async fn get_json(app: &axum::Router, uri: &str) -> serde_json::Value {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-fabro-demo", "1")
        .body(Body::empty())
        .expect("event pagination request should build");
    let response = app.clone().oneshot(req).await.unwrap();
    response_json(response, StatusCode::OK, format!("GET {uri}")).await
}

#[tokio::test]
async fn demo_stage_events_default_returns_all_fixture_events_with_no_more() {
    let app = fabro_server::test_support::build_test_router(test_app_state());

    let body = get_json(&app, "/api/v1/runs/run-1/stages/detect-drift/events").await;
    let data = body["data"].as_array().expect("data is an array");

    assert_eq!(data.len(), 7, "all seven fixture events should be returned");
    assert_eq!(body["meta"]["has_more"], false);
}

#[tokio::test]
async fn demo_stage_events_limit_one_signals_has_more() {
    let app = fabro_server::test_support::build_test_router(test_app_state());

    let body = get_json(
        &app,
        "/api/v1/runs/run-1/stages/detect-drift/events?limit=1",
    )
    .await;
    let data = body["data"].as_array().expect("data is an array");

    assert_eq!(data.len(), 1);
    assert_eq!(body["meta"]["has_more"], true);
}

#[tokio::test]
async fn demo_stage_events_since_seq_filters_out_earlier_events() {
    let app = fabro_server::test_support::build_test_router(test_app_state());

    // The fixture seqs are 1..=7. since_seq=4 should skip the first three.
    let body = get_json(
        &app,
        "/api/v1/runs/run-1/stages/detect-drift/events?since_seq=4",
    )
    .await;
    let data = body["data"].as_array().expect("data is an array");

    assert_eq!(data.len(), 4);
    let seqs: Vec<u64> = data
        .iter()
        .map(|envelope| envelope["seq"].as_u64().expect("seq is a number"))
        .collect();
    assert_eq!(seqs, vec![4, 5, 6, 7]);
    assert_eq!(body["meta"]["has_more"], false);
}
