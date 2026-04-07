use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::time::sleep;
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, POLL_ATTEMPTS, POLL_INTERVAL, api, body_json, create_and_start_run,
    dry_run_settings, test_app_state_with_options, test_app_with_scheduler, wait_for_run_status,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aggregate_billing_increments_after_run_completes() {
    let state = test_app_state_with_options(dry_run_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id = create_and_start_run(&app, MINIMAL_DOT).await;

    // Poll until run completes
    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(status, "succeeded");

    let mut total_runs = 0;
    for _ in 0..POLL_ATTEMPTS {
        let req = Request::builder()
            .method("GET")
            .uri(api("/billing"))
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        total_runs = body["totals"]["runs"].as_i64().unwrap();
        if total_runs == 1 {
            break;
        }
        sleep(POLL_INTERVAL).await;
    }
    assert_eq!(total_runs, 1);
}
