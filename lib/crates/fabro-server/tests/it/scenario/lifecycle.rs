use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_interview::Interviewer;
use fabro_server::server::{build_router, create_app_state_with_registry_factory};
use fabro_workflow::handler::HandlerRegistry;
use fabro_workflow::handler::agent::AgentHandler;
use fabro_workflow::handler::exit::ExitHandler;
use fabro_workflow::handler::human::HumanHandler;
use fabro_workflow::handler::start::StartHandler;
use tower::ServiceExt;

use crate::helpers::{
    POLL_ATTEMPTS, POLL_INTERVAL, api, body_json, run_json, wait_for_run_status,
    wait_for_run_status_not_in,
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
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let arr = body["data"].as_array().unwrap();
        if let Some(question_id) = arr
            .first()
            .and_then(|item| item["id"].as_str())
            .map(ToOwned::to_owned)
        {
            return question_id;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("question should have appeared");
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
    let state = create_app_state_with_registry_factory(gate_registry);
    fabro_server::server::spawn_scheduler(Arc::clone(&state));
    let app = build_router(
        Arc::clone(&state),
        fabro_server::jwt_auth::AuthMode::Disabled,
    );

    // 1. Create run
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({"dot_source": GATE_DOT})).unwrap(),
        ))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap().to_string();

    // 1b. Start the run
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/start")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 2. Poll for question to appear (run goes start -> work -> gate, then blocks)
    let question_id = wait_for_question_id(&app, &run_id).await;

    // 3. Submit answer selecting first option (Approve)
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!(
            "/runs/{run_id}/questions/{question_id}/answer"
        )))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({"value": "A"})).unwrap(),
        ))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

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
    let body = body_json(response.into_body()).await;
    assert!(
        body["data"].as_array().unwrap().is_empty(),
        "no pending questions after completion"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_http_lifecycle_cancel() {
    let state = create_app_state_with_registry_factory(gate_registry);
    fabro_server::server::spawn_scheduler(Arc::clone(&state));
    let app = build_router(
        Arc::clone(&state),
        fabro_server::jwt_auth::AuthMode::Disabled,
    );

    // Create and start a run that will block at the human gate
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({"dot_source": GATE_DOT})).unwrap(),
        ))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let run_id = body["id"].as_str().unwrap().to_string();

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/start")))
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(req).await.unwrap();

    // Subscribe as soon as the scheduler has created the live event stream.
    // Waiting past "starting" races with stage events because `/events`
    // subscribes to future broadcast messages only; it does not replay.
    wait_for_run_status_not_in(&app, &run_id, &["queued"]).await;

    // Cancel it
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/cancel")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response.into_body()).await;
    assert_eq!(body["status"], "cancelled");

    // Verify the durable store view converges to cancelled failure.
    let status = wait_for_run_status(&app, &run_id, &["failed"]).await;
    assert_eq!(status, "failed");
    let body = run_json(&app, &run_id).await;
    assert_eq!(body["status_reason"], "cancelled");
}
