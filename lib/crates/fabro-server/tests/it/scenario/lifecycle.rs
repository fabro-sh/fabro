use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_interview::Interviewer;
use fabro_server::server::{
    create_app_state_with_runtime_settings_and_registry_factory, spawn_scheduler,
};
use fabro_workflow::handler::HandlerRegistry;
use fabro_workflow::handler::agent::AgentHandler;
use fabro_workflow::handler::exit::ExitHandler;
use fabro_workflow::handler::human::HumanHandler;
use fabro_workflow::handler::start::StartHandler;
use tokio::time::sleep;
use tower::ServiceExt;

use crate::helpers::{
    POLL_ATTEMPTS, POLL_INTERVAL, api, minimal_manifest_json, response_json, response_status,
    run_json, test_settings, wait_for_run_status,
};

fn gate_registry(interviewer: Arc<dyn Interviewer>) -> HandlerRegistry {
    let mut registry = HandlerRegistry::new(Box::new(AgentHandler::new(None)));
    registry.register("start", Box::new(StartHandler));
    registry.register("exit", Box::new(ExitHandler));
    registry.register("agent", Box::new(AgentHandler::new(None)));
    registry.register("human", Box::new(HumanHandler::new(interviewer)));
    registry
}

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
        if body["status"]["kind"].as_str() == Some(expected_status)
            && body["status"]["reason"].as_str() == Some(expected_reason)
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
    let settings = test_settings();
    let state = create_app_state_with_runtime_settings_and_registry_factory(
        settings.server_settings,
        settings.manifest_run_defaults,
        gate_registry,
    );
    spawn_scheduler(Arc::clone(&state));
    let app = fabro_server::test_support::build_test_router(Arc::clone(&state));

    // 1. Create run
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&minimal_manifest_json(GATE_DOT)).unwrap(),
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
    assert_eq!(question["stage"], "gate");
    assert!(question["timeout_seconds"].is_null());
    assert!(question["context_display"].is_null() || question["context_display"].is_string());

    // 3. Submit answer selecting first option (Approve)
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!(
            "/runs/{run_id}/questions/{question_id}/answer"
        )))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({"selected_option_key": "A"})).unwrap(),
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
    let settings = test_settings();
    let state = create_app_state_with_runtime_settings_and_registry_factory(
        settings.server_settings,
        settings.manifest_run_defaults,
        gate_registry,
    );
    spawn_scheduler(Arc::clone(&state));
    let app = fabro_server::test_support::build_test_router(Arc::clone(&state));

    // Create and start a run that will block at the human gate
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&minimal_manifest_json(GATE_DOT)).unwrap(),
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
        StatusCode::OK,
        format!("POST /api/v1/runs/{run_id}/cancel"),
    )
    .await;
    assert_eq!(body["status"]["kind"], "running");
    // `pending_control` is computed from the store projection after the cancel
    // event is appended AND the worker is signaled. The worker is sitting at a
    // human gate; once notified it can emit a clearing event before this
    // handler re-reads the projection, so the response can legitimately
    // observe either the still-pending "cancel" or a null where the worker
    // already consumed it. Durable convergence is asserted below.
    let pending_control = &body["pending_control"];
    assert!(
        pending_control == "cancel" || pending_control.is_null(),
        "expected pending_control to be \"cancel\" or null, got {pending_control}"
    );

    // Verify the durable store view converges to cancelled failure.
    let body = wait_for_run_state(&app, &run_id, "failed", "cancelled").await;
    assert_eq!(body["status"]["reason"], "cancelled");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_at_human_gate_persists_cancelled_terminal_event() {
    let settings = test_settings();
    let state = create_app_state_with_runtime_settings_and_registry_factory(
        settings.server_settings,
        settings.manifest_run_defaults,
        gate_registry,
    );
    spawn_scheduler(Arc::clone(&state));
    let app = fabro_server::test_support::build_test_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&minimal_manifest_json(GATE_DOT)).unwrap(),
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
        StatusCode::OK,
        format!("POST /api/v1/runs/{run_id}/cancel"),
    )
    .await;

    let status = wait_for_run_status(&app, &run_id, &["failed"]).await;
    assert_eq!(status, "failed");

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/events")))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body = response_json(
        response,
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/events"),
    )
    .await;
    let failed_reasons = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|&event| event["event"] == "run.failed")
        .map(|event| {
            (
                event["properties"]["reason"]
                    .as_str()
                    .map(ToOwned::to_owned),
                event["properties"]["error"].as_str().map(ToOwned::to_owned),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(failed_reasons, vec![(
        Some("cancelled".to_string()),
        Some("Pipeline cancelled".to_string())
    )]);
}
