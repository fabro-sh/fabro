use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::time::sleep;
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, POLL_ATTEMPTS, POLL_INTERVAL, api, body_json, create_and_start_run_from_manifest,
    minimal_manifest_json_with_dry_run, test_app_state_with_options, test_app_with_scheduler,
    test_settings, wait_for_run_status,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aggregate_billing_increments_after_run_completes() {
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id =
        create_and_start_run_from_manifest(&app, minimal_manifest_json_with_dry_run(MINIMAL_DOT))
            .await;

    // Poll until run completes
    let status = wait_for_run_status(&app, &run_id, &["completed", "failed"]).await;
    assert_eq!(status, "completed");

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
