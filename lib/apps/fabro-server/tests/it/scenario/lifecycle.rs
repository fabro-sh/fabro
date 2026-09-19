use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_server::server::spawn_scheduler;
use fabro_server::test_support::test_app_state_with_runtime_settings_in_process;
use tokio::time::sleep;
use tower::ServiceExt;

use crate::helpers::{
    POLL_ATTEMPTS, POLL_INTERVAL, api, minimal_intent_json, response_json, response_status,
    run_json, test_settings, wait_for_run_status,
};

async fn wait_for_question_id(app: &axum::Router, run_id: &str) -> String {
    for _ in 0..POLL_ATTEMPTS {
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/questions")))
            .body(Body::empty())
            .expect("questions request should build");
        let response = app.clone().oneshot(req).await.unwrap();
        let body = response_json(
            response,
            StatusCode::OK,
            format!("GET /api/v1/runs/{run_id}/questions"),
        )
        .await;
        let arr = body["data"]
            .as_array()
            .expect("questions response should include a data array");
        if let Some(question_id) = arr
            .first()
            .and_then(|item| item["id"].as_str())
            .map(ToOwned::to_owned)
        {
            return question_id;
        }
        sleep(POLL_INTERVAL).await;
    }
    panic!("question should have appeared");
}

async fn wait_for_question(app: &axum::Router, run_id: &str) -> serde_json::Value {
    for _ in 0..POLL_ATTEMPTS {
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/questions")))
            .body(Body::empty())
            .expect("questions request should build");
        let response = app.clone().oneshot(req).await.unwrap();
        let body = response_json(
            response,
            StatusCode::OK,
            format!("GET /api/v1/runs/{run_id}/questions"),
        )
        .await;
        let arr = body["data"]
            .as_array()
            .expect("questions response should include a data array");
        if let Some(question) = arr.first() {
            return question.clone();
        }
        sleep(POLL_INTERVAL).await;
    }
    panic!("question should have appeared");
}

async fn wait_for_run_state(
    app: &axum::Router,
    run_id: &str,
    expected_status: &str,
    expected_reason: &str,
) -> serde_json::Value {
    for _ in 0..POLL_ATTEMPTS {
        let body = run_json(app, run_id).await;
        if body["lifecycle"]["status"]["kind"].as_str() == Some(expected_status)
            && body["lifecycle"]["status"]["reason"].as_str() == Some(expected_reason)
        {
            return body;
        }
        sleep(POLL_INTERVAL).await;
    }
    panic!("run {run_id} did not reach status={expected_status} reason={expected_reason}");
}

const GATE_DOT: &str = r#"digraph GateTest {
    graph [goal="Test gate"]
    start [shape=Mdiamond]
    exit  [shape=Msquare]
    work  [shape=box, prompt="Do work"]
    gate  [shape=hexagon, type="human", label="Approve?"]
    done  [shape=box, prompt="Finish"]
    revise [shape=box, prompt="Revise"]

    start -> work -> gate
    gate -> done   [label="[A] Approve"]
    gate -> revise [label="[R] Revise"]
    done -> exit
    revise -> gate
}"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_http_lifecycle_approve_and_complete() {
    let workspace = tempfile::tempdir().unwrap();
    let settings = test_settings();
    let state = test_app_state_with_runtime_settings_in_process(
        settings.server_settings,
        settings.manifest_run_defaults,
    );
    spawn_scheduler(Arc::clone(&state));
    let app = fabro_server::test_support::build_test_router(Arc::clone(&state));

    // 1. Create run
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&minimal_intent_json(&app, GATE_DOT, workspace.path()).await)
                .unwrap(),
        ))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json(response, StatusCode::CREATED, "POST /api/v1/runs").await;
    let run_id = body["id"].as_str().unwrap().to_string();

    // 1b. Start the run
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/start")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    response_status(
        response,
        StatusCode::OK,
        format!("POST /api/v1/runs/{run_id}/start"),
    )
    .await;

    // 2. Poll for question to appear (run goes start -> work -> gate, then blocks)
    let question = wait_for_question(&app, &run_id).await;
    let question_id = question["id"].as_str().unwrap().to_string();
    assert_eq!(question["stage"], "gate@1");
    assert!(question["timeout_seconds"].is_null());
    assert!(question["context_display"].is_null() || question["context_display"].is_string());

    // 3. Submit answer selecting first option (Approve). Petri's id
    // (`gate#3`) travels as one percent-encoded path segment.
    let encoded_id =
        percent_encoding::utf8_percent_encode(&question_id, percent_encoding::NON_ALPHANUMERIC)
            .to_string();
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!(
            "/runs/{run_id}/questions/{encoded_id}/answer"
        )))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "kind": "selected",
                "option_key": "A",
            }))
            .unwrap(),
        ))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    response_status(
        response,
        StatusCode::NO_CONTENT,
        format!("POST /api/v1/runs/{run_id}/questions/{question_id}/answer"),
    )
    .await;

    // 4. Poll until the run reaches a terminal success or failure state.
    let final_status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(final_status, "succeeded");

    // 5. Verify no pending questions
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/questions")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json(
        response,
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/questions"),
    )
    .await;
    assert!(
        body["data"].as_array().unwrap().is_empty(),
        "no pending questions after completion"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_http_lifecycle_cancel() {
    let workspace = tempfile::tempdir().unwrap();
    let settings = test_settings();
    let state = test_app_state_with_runtime_settings_in_process(
        settings.server_settings,
        settings.manifest_run_defaults,
    );
    spawn_scheduler(Arc::clone(&state));
    let app = fabro_server::test_support::build_test_router(Arc::clone(&state));

    // Create and start a run that will block at the human gate
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&minimal_intent_json(&app, GATE_DOT, workspace.path()).await)
                .unwrap(),
        ))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json(response, StatusCode::CREATED, "POST /api/v1/runs").await;
    let run_id = body["id"].as_str().unwrap().to_string();

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/start")))
        .body(Body::empty())
        .unwrap();
    response_status(
        app.clone().oneshot(req).await.unwrap(),
        StatusCode::OK,
        format!("POST /api/v1/runs/{run_id}/start"),
    )
    .await;

    // Wait until the worker has reached the human gate so cancel exercises the
    // live-running path rather than racing the in-memory queue transition.
    let _question_id = wait_for_question_id(&app, &run_id).await;

    // Cancel it
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/cancel")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json(
        response,
        StatusCode::ACCEPTED,
        format!("POST /api/v1/runs/{run_id}/cancel"),
    )
    .await;
    // Both fields below are read from the same post-signal projection, so both
    // race the worker the same way. `status.kind` is "blocked" while the worker
    // still sits at the gate and "running" once it has been notified and
    // resumed to process the cancel. What matters here is that cancel reached a
    // live run rather than racing the in-memory queue transition, so this
    // asserts "not queued" via the two live states. Durable convergence is
    // asserted below.
    let status_kind = &body["lifecycle"]["status"]["kind"];
    assert!(
        status_kind == "blocked" || status_kind == "running",
        "expected status.kind to be \"blocked\" or \"running\", got {status_kind}"
    );
    // `pending_control` is computed from the store projection after the cancel
    // event is appended AND the worker is signaled. The worker is sitting at a
    // human gate; once notified it can emit a clearing event before this
    // handler re-reads the projection, so the response can legitimately
    // observe either the still-pending "cancel" or a null where the worker
    // already consumed it. Durable convergence is asserted below.
    let pending_control = &body["lifecycle"]["pending_control"];
    assert!(
        pending_control == "cancel" || pending_control.is_null(),
        "expected pending_control to be \"cancel\" or null, got {pending_control}"
    );

    // Verify the durable store view converges to cancelled failure.
    let body = wait_for_run_state(&app, &run_id, "failed", "cancelled").await;
    assert_eq!(body["lifecycle"]["status"]["reason"], "cancelled");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_at_human_gate_persists_cancelled_terminal_event() {
    let workspace = tempfile::tempdir().unwrap();
    let settings = test_settings();
    let state = test_app_state_with_runtime_settings_in_process(
        settings.server_settings,
        settings.manifest_run_defaults,
    );
    spawn_scheduler(Arc::clone(&state));
    let app = fabro_server::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&minimal_intent_json(&app, GATE_DOT, workspace.path()).await)
                .unwrap(),
        ))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = response_json(response, StatusCode::CREATED, "POST /api/v1/runs").await;
    let run_id = body["id"].as_str().unwrap().to_string();

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/start")))
        .body(Body::empty())
        .unwrap();
    response_status(
        app.clone().oneshot(req).await.unwrap(),
        StatusCode::OK,
        format!("POST /api/v1/runs/{run_id}/start"),
    )
    .await;

    let _question_id = wait_for_question_id(&app, &run_id).await;

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/cancel")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    response_status(
        response,
        StatusCode::ACCEPTED,
        format!("POST /api/v1/runs/{run_id}/cancel"),
    )
    .await;

    let status = wait_for_run_status(&app, &run_id, &["failed"]).await;
    assert_eq!(status, "failed");

    // The run's record says it was cancelled: Petri's finish, and the
    // terminal lifecycle record Fabro wrote after it, both name the reason.
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json(
        response,
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}"),
    )
    .await;
    assert_eq!(body["lifecycle"]["status"]["reason"], "cancelled", "{body}");
}
