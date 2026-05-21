use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, minimal_manifest_json, response_json, response_status, test_app_state,
};

async fn create_run(app: &axum::Router) -> String {
    let request = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&minimal_manifest_json(MINIMAL_DOT))
                .expect("manifest should serialize"),
        ))
        .expect("create-run request should build");
    let body = response_json(
        app.clone().oneshot(request).await.unwrap(),
        StatusCode::CREATED,
        "POST /api/v1/runs",
    )
    .await;
    body["id"]
        .as_str()
        .expect("create-run response should include an id")
        .to_string()
}

async fn create_session(app: &axum::Router, run_id: &str, title: &str) -> serde_json::Value {
    let request = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/sessions")))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({ "title": title }))
                .expect("session request should serialize"),
        ))
        .expect("create-session request should build");
    response_json(
        app.clone().oneshot(request).await.unwrap(),
        StatusCode::CREATED,
        format!("POST /api/v1/runs/{run_id}/sessions"),
    )
    .await
}

#[tokio::test]
async fn run_bound_session_is_created_as_run_event_and_resolves_by_flat_id() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let run_id = create_run(&app).await;

    let created = create_session(&app, &run_id, "Ask Fabro").await;
    let session_id = created["id"]
        .as_str()
        .expect("session response should include an id");
    assert_eq!(created["run_id"], run_id);
    assert_eq!(created["title"], "Ask Fabro");
    assert_session_metadata_only(&created);
    assert!(session_id.parse::<fabro_types::SessionId>().is_ok());

    let get_request = Request::builder()
        .method("GET")
        .uri(api(&format!("/sessions/{session_id}")))
        .body(Body::empty())
        .expect("get-session request should build");
    let fetched = response_json(
        app.clone().oneshot(get_request).await.unwrap(),
        StatusCode::OK,
        format!("GET /api/v1/sessions/{session_id}"),
    )
    .await;
    assert_eq!(fetched["id"], session_id);
    assert_eq!(fetched["run_id"], run_id);
    assert_session_metadata_only(&fetched);

    let events_request = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/events")))
        .body(Body::empty())
        .expect("run-events request should build");
    let events = response_json(
        app.clone().oneshot(events_request).await.unwrap(),
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/events"),
    )
    .await;
    let session_events: Vec<_> = events["data"]
        .as_array()
        .expect("events response should include data")
        .iter()
        .filter(|event| event["session_id"] == session_id)
        .collect();
    assert_eq!(session_events.len(), 1);
    assert_eq!(session_events[0]["event"], "run.session.created");
    assert!(
        session_events[0]["properties"].get("permissions").is_none(),
        "run session creation event should not expose permissions"
    );
}

#[tokio::test]
async fn sessions_are_listed_only_under_their_owning_run() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let first_run_id = create_run(&app).await;
    let second_run_id = create_run(&app).await;
    let created = create_session(&app, &first_run_id, "First run chat").await;

    let first_request = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{first_run_id}/sessions")))
        .body(Body::empty())
        .expect("list sessions request should build");
    let first = response_json(
        app.clone().oneshot(first_request).await.unwrap(),
        StatusCode::OK,
        format!("GET /api/v1/runs/{first_run_id}/sessions"),
    )
    .await;
    assert_eq!(first["data"].as_array().unwrap().len(), 1);
    assert_eq!(first["data"][0]["id"], created["id"]);
    assert_session_metadata_only(&first["data"][0]);

    let second_request = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{second_run_id}/sessions")))
        .body(Body::empty())
        .expect("list sessions request should build");
    let second = response_json(
        app.clone().oneshot(second_request).await.unwrap(),
        StatusCode::OK,
        format!("GET /api/v1/runs/{second_run_id}/sessions"),
    )
    .await;
    assert!(second["data"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn session_metadata_patch_route_is_removed() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let run_id = create_run(&app).await;
    let created = create_session(&app, &run_id, "Ask Fabro").await;
    let session_id = created["id"]
        .as_str()
        .expect("session response should include an id");

    let request = Request::builder()
        .method("PATCH")
        .uri(api(&format!("/sessions/{session_id}")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"title":"Renamed"}"#))
        .expect("patch-session request should build");
    response_status(
        app.clone().oneshot(request).await.unwrap(),
        StatusCode::NOT_FOUND,
        format!("PATCH /api/v1/sessions/{session_id}"),
    )
    .await;
}

#[tokio::test]
async fn derived_session_read_routes_are_removed() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let run_id = create_run(&app).await;
    let created = create_session(&app, &run_id, "Ask Fabro").await;
    let session_id = created["id"]
        .as_str()
        .expect("session response should include an id");
    let turn_id = fabro_types::TurnId::new();

    for path in [
        format!("/sessions/{session_id}/turns"),
        format!("/sessions/{session_id}/turns/{turn_id}"),
        format!("/sessions/{session_id}/events"),
    ] {
        let request = Request::builder()
            .method("GET")
            .uri(api(&path))
            .body(Body::empty())
            .expect("removed session read request should build");
        response_status(
            app.clone().oneshot(request).await.unwrap(),
            StatusCode::NOT_FOUND,
            format!("GET /api/v1{path}"),
        )
        .await;
    }
}

#[tokio::test]
async fn inactive_turn_interrupt_returns_conflict() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let run_id = create_run(&app).await;
    let created = create_session(&app, &run_id, "Ask Fabro").await;
    let session_id = created["id"]
        .as_str()
        .expect("session response should include an id");
    let turn_id = fabro_types::TurnId::new();

    let request = Request::builder()
        .method("POST")
        .uri(api(&format!(
            "/sessions/{session_id}/turns/{turn_id}/interrupt"
        )))
        .body(Body::empty())
        .expect("interrupt request should build");
    response_status(
        app.clone().oneshot(request).await.unwrap(),
        StatusCode::CONFLICT,
        format!("POST /api/v1/sessions/{session_id}/turns/{turn_id}/interrupt"),
    )
    .await;
}

fn assert_session_metadata_only(value: &serde_json::Value) {
    let object = value
        .as_object()
        .expect("session response should be a JSON object");
    for field in [
        "working_dir",
        "provider",
        "permissions",
        "deleted_at",
        "runtime_context",
    ] {
        assert!(
            !object.contains_key(field),
            "session metadata should not expose {field}"
        );
    }
}
