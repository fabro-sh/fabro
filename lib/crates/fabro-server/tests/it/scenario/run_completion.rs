use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::time::sleep;
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, create_and_start_run, dry_run_settings, test_app_state_with_options,
    test_app_with_scheduler, wait_for_run_status,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_completes_and_status_is_completed() {
    let state = test_app_state_with_options(dry_run_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id = create_and_start_run(&app, MINIMAL_DOT).await;

    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(status, "succeeded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_run_events_returns_sse_stream() {
    let state = test_app_state_with_options(dry_run_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id = create_and_start_run(&app, MINIMAL_DOT).await;

    // Wait for scheduler to promote run.
    sleep(std::time::Duration::from_millis(100)).await;

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/attach")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let status = response.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::GONE,
        "unexpected status: {status}"
    );

    if status == StatusCode::OK {
        let content_type = response
            .headers()
            .get("content-type")
            .expect("content-type header should be present")
            .to_str()
            .unwrap();
        assert!(
            content_type.contains("text/event-stream"),
            "expected text/event-stream, got: {content_type}"
        );
    }
}
