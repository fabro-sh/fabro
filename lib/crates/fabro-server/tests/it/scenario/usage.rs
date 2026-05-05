use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::time::sleep;
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, POLL_ATTEMPTS, POLL_INTERVAL, api, create_and_start_run_from_manifest,
    minimal_manifest_json, minimal_manifest_json_with_dry_run, test_app_state_with_options,
    test_app_with_scheduler, test_settings, wait_for_run_status,
};

const COMMAND_DOT: &str = r#"digraph Test {
    graph [goal="Test"]
    start [shape=Mdiamond]
    echo_task [shape=parallelogram, script="echo command-stage"]
    exit  [shape=Msquare]
    start -> echo_task -> exit
}"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aggregate_billing_increments_after_run_completes() {
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id =
        create_and_start_run_from_manifest(&app, minimal_manifest_json_with_dry_run(MINIMAL_DOT))
            .await;

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
        let body = crate::helpers::response_json(
            response,
            StatusCode::OK,
            format!("{}:{}", file!(), line!()),
        )
        .await;
        total_runs = body["totals"]["runs"].as_i64().unwrap();
        if total_runs == 1 {
            break;
        }
        sleep(POLL_INTERVAL).await;
    }
    assert_eq!(total_runs, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_billing_includes_completed_non_llm_stages() {
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id =
        create_and_start_run_from_manifest(&app, minimal_manifest_json_with_dry_run(MINIMAL_DOT))
            .await;

    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(status, "succeeded");

    let billing = run_billing(&app, &run_id).await;
    assert_non_llm_billing(&billing, &["start"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_billing_includes_completed_command_stages() {
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id = create_and_start_run_from_manifest(&app, minimal_manifest_json(COMMAND_DOT)).await;

    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(status, "succeeded");

    let billing = run_billing(&app, &run_id).await;
    assert_non_llm_billing(&billing, &["echo_task", "start"]);
}

async fn run_billing(app: &axum::Router, run_id: &str) -> serde_json::Value {
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/billing")))
        .body(Body::empty())
        .expect("run billing request should build");

    let response = app.clone().oneshot(req).await.unwrap();
    crate::helpers::response_json(
        response,
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/billing"),
    )
    .await
}

fn assert_non_llm_billing(billing: &serde_json::Value, expected_stage_ids: &[&str]) {
    let stages = billing["stages"]
        .as_array()
        .expect("billing response should include stages");
    let mut stage_ids = stages
        .iter()
        .map(|stage| {
            stage["stage"]["id"]
                .as_str()
                .expect("stage should include an id")
                .to_string()
        })
        .collect::<Vec<_>>();
    stage_ids.sort();
    assert_eq!(stage_ids, expected_stage_ids);

    assert!(
        stages.iter().all(|stage| {
            stage["model"].is_null()
                && stage["billing"]["input_tokens"] == 0
                && stage["billing"]["output_tokens"] == 0
                && stage["billing"]["reasoning_tokens"] == 0
                && stage["billing"]["total_usd_micros"].is_null()
        }),
        "every non-LLM stage should have null model and zero token counts: {stages:?}"
    );

    let runtime_secs: f64 = stages
        .iter()
        .map(|stage| {
            stage["runtime_secs"]
                .as_f64()
                .expect("stage should include runtime_secs")
        })
        .sum();

    assert_eq!(
        billing["by_model"]
            .as_array()
            .expect("billing response should include by_model")
            .len(),
        0
    );
    assert_eq!(billing["totals"]["input_tokens"], 0);
    assert_eq!(billing["totals"]["output_tokens"], 0);
    assert!(billing["totals"]["total_usd_micros"].is_null());

    let total_runtime_secs = billing["totals"]["runtime_secs"]
        .as_f64()
        .expect("totals should include runtime_secs");
    assert!(
        (total_runtime_secs - runtime_secs).abs() < f64::EPSILON,
        "total runtime {total_runtime_secs} should equal summed stage runtime {runtime_secs}"
    );
}
