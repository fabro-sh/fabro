use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, body_json, create_and_start_run, dry_run_app, wait_for_run_status,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_serve_starts_and_runs_workflow() {
    let app = dry_run_app();

    let run_id = create_and_start_run(&app, MINIMAL_DOT).await;

    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(status, "succeeded");
}

#[tokio::test]
async fn test_model_known_via_full_router() {
    let app = dry_run_app();

    let req = Request::builder()
        .method("POST")
        .uri(api("/models/claude-opus-4-6/test"))
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response.into_body()).await;
    assert_eq!(body["model_id"], "claude-opus-4-6");
    // No API keys in test env, so status will be "error"
    assert!(body["status"] == "ok" || body["status"] == "error");
}

#[tokio::test]
async fn test_model_unknown_via_full_router() {
    let app = dry_run_app();

    let req = Request::builder()
        .method("POST")
        .uri(api("/models/nonexistent-model-xyz/test"))
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn dry_run_serve_rejects_invalid_dot() {
    let app = dry_run_app();

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({"dot_source": "not valid dot"})).unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
