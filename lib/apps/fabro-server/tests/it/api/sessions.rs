use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use fabro_static::EnvVars;
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, minimal_intent_json, response_json, response_status, test_app_state,
};

async fn create_run(app: &axum::Router) -> (String, tempfile::TempDir) {
    let workspace = tempfile::tempdir().expect("run target workspace should be created");
    let request = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&minimal_intent_json(app, MINIMAL_DOT, workspace.path()).await)
                .expect("intent should serialize"),
        ))
        .expect("create-run request should build");
    let body = response_json(
        app.clone().oneshot(request).await.unwrap(),
        StatusCode::CREATED,
        "POST /api/v1/runs",
    )
    .await;
    let run_id = body["id"]
        .as_str()
        .expect("create-run response should include an id")
        .to_string();
    (run_id, workspace)
}

async fn create_session(app: &axum::Router, run_id: &str, title: &str) -> serde_json::Value {
    create_session_with_body(app, run_id, serde_json::json!({ "title": title })).await
}

async fn create_session_with_model(
    app: &axum::Router,
    run_id: &str,
    title: &str,
    model: &str,
) -> serde_json::Value {
    create_session_with_body(
        app,
        run_id,
        serde_json::json!({ "title": title, "model": model }),
    )
    .await
}

async fn create_session_with_body(
    app: &axum::Router,
    run_id: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    response_json(
        create_session_response(app, run_id, body).await,
        StatusCode::CREATED,
        format!("POST /api/v1/runs/{run_id}/sessions"),
    )
    .await
}

async fn create_session_response(
    app: &axum::Router,
    run_id: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    let request = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/sessions")))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&body).expect("session request should serialize"),
        ))
        .expect("create-session request should build");
    app.clone().oneshot(request).await.unwrap()
}

#[tokio::test]
async fn run_bound_session_is_created_with_a_session_event_and_resolves_by_flat_id() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let (run_id, _run_id_workspace) = create_run(&app).await;

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
    assert!(
        fetched.get("messages").is_none(),
        "the conversation is held by the server's session record, not the API"
    );
    assert!(fetched["active_turn"].is_null());

    let events_request = Request::builder()
        .method("GET")
        .uri(api(&format!("/sessions/{session_id}/events")))
        .body(Body::empty())
        .expect("session-events request should build");
    let events = response_json(
        app.clone().oneshot(events_request).await.unwrap(),
        StatusCode::OK,
        format!("GET /api/v1/sessions/{session_id}/events"),
    )
    .await;
    let session_events: Vec<_> = events["data"]
        .as_array()
        .expect("events response should include data")
        .iter()
        .filter(|event| event["session_id"] == session_id)
        .collect();
    assert_eq!(session_events.len(), 1);
    assert_eq!(fetched["last_seq"], session_events[0]["seq"]);
    assert_eq!(session_events[0]["event"], "run.session.created");
    assert!(
        session_events[0]["properties"].get("permissions").is_none(),
        "run session creation event should not expose permissions"
    );
}

#[tokio::test]
async fn sessions_are_listed_only_under_their_owning_run() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let (first_run_id, _first_run_id_workspace) = create_run(&app).await;
    let (second_run_id, _second_run_id_workspace) = create_run(&app).await;
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
async fn supplied_session_model_alias_is_canonicalized() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let (run_id, _run_id_workspace) = create_run(&app).await;

    let created = create_session_with_model(&app, &run_id, "Ask Fabro", "gpt54").await;
    assert_eq!(created["model"], "gpt-5.4");
    let session_id = created["id"].as_str().expect("session id");

    let events_request = Request::builder()
        .method("GET")
        .uri(api(&format!("/sessions/{session_id}/events")))
        .body(Body::empty())
        .expect("session-events request should build");
    let events = response_json(
        app.clone().oneshot(events_request).await.unwrap(),
        StatusCode::OK,
        format!("GET /api/v1/sessions/{session_id}/events"),
    )
    .await;

    let created_event = events["data"]
        .as_array()
        .expect("events response should include data")
        .iter()
        .find(|event| event["event"] == "run.session.created")
        .expect("session creation event should be recorded");
    assert_eq!(created_event["properties"]["model"], "gpt-5.4");
    assert_eq!(created_event["properties"]["provider"], "openai");
}

#[tokio::test]
async fn provider_qualified_session_model_is_canonicalized() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let (run_id, _run_id_workspace) = create_run(&app).await;

    let created =
        create_session_with_model(&app, &run_id, "Ask Fabro", "openai/gpt-5.4-mini").await;

    assert_eq!(created["model"], "gpt-5.4-mini");
    assert_eq!(created["provider"], "openai");
}

#[tokio::test]
async fn unknown_session_models_preserve_passthrough_on_the_selected_provider() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let (run_id, _run_id_workspace) = create_run(&app).await;

    for model in ["not-a-real-model", "openai/not-a-real-model"] {
        let created = create_session_with_model(&app, &run_id, "Ask Fabro", model).await;
        assert_eq!(created["model"], "not-a-real-model");
        assert_eq!(created["provider"], "openai");
    }
}

#[tokio::test]
async fn invalid_session_model_refs_are_rejected_at_creation() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let (run_id, _run_id_workspace) = create_run(&app).await;

    for model in ["openai", "openai/", "anthropic/gpt-5.4"] {
        let response = create_session_response(
            &app,
            &run_id,
            serde_json::json!({ "title": "Ask Fabro", "model": model }),
        )
        .await;
        response_status(
            response,
            StatusCode::BAD_REQUEST,
            format!("POST /api/v1/runs/{run_id}/sessions with model {model}"),
        )
        .await;
    }
}

#[tokio::test]
async fn ambiguous_session_model_refs_are_rejected_at_creation() {
    let state = fabro_server::test_support::TestAppStateBuilder::new()
        .llm_overlay_toml("[providers.openai]\naliases = [\"gpt54\"]\n")
        .vault_entries([(EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
        .build();
    let app = fabro_server::test_support::build_test_router(state);
    let (run_id, _run_id_workspace) = create_run(&app).await;

    let response = create_session_response(
        &app,
        &run_id,
        serde_json::json!({ "title": "Ask Fabro", "model": "gpt54" }),
    )
    .await;
    response_status(
        response,
        StatusCode::BAD_REQUEST,
        format!("POST /api/v1/runs/{run_id}/sessions with ambiguous model gpt54"),
    )
    .await;
}

#[tokio::test]
async fn session_turn_fails_when_selected_model_provider_becomes_unconfigured() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let (run_id, _run_id_workspace) = create_run(&app).await;
    let created = create_session_with_model(&app, &run_id, "Ask Fabro", "gpt54").await;
    let session_id = created["id"]
        .as_str()
        .expect("session response should include an id");
    delete_openai_credential(&app).await;

    let request = Request::builder()
        .method("POST")
        .uri(api(&format!("/sessions/{session_id}/turns")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"input":"Which provider are you using?"}"#))
        .expect("submit-turn request should build");
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().contains_key("x-fabro-turn-id"),
        "submit turn should return the generated turn id"
    );
    let events = session_sse_events(response).await;

    let failed = events
        .iter()
        .find(|event| event["event"] == "run.session.turn.failed")
        .expect("selected provider failure should be streamed");
    assert!(
        failed["properties"]["error"]
            .as_str()
            .expect("failure event should include an error")
            .contains("provider 'openai'"),
        "failure should be for the selected model provider: {failed:?}"
    );
    assert_eq!(failed["properties"]["code"], "model_unavailable");
    assert_eq!(failed["properties"]["retryable"], false);
}

#[tokio::test]
async fn session_metadata_patch_route_is_removed() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let (run_id, _run_id_workspace) = create_run(&app).await;
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
async fn unsupported_derived_turn_read_routes_are_removed() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let (run_id, _run_id_workspace) = create_run(&app).await;
    let created = create_session(&app, &run_id, "Ask Fabro").await;
    let session_id = created["id"]
        .as_str()
        .expect("session response should include an id");
    let turn_id = fabro_types::TurnId::new();

    for path in [format!("/sessions/{session_id}/turns/{turn_id}")] {
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
async fn session_events_are_listed_by_session_and_paginated_by_session_sequence() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let (run_id, _run_id_workspace) = create_run(&app).await;
    let first = create_session(&app, &run_id, "First").await;
    let second = create_session(&app, &run_id, "Second").await;
    let first_id = first["id"].as_str().unwrap();
    let second_id = second["id"].as_str().unwrap();
    let get_first = Request::builder()
        .method("GET")
        .uri(api(&format!("/sessions/{first_id}")))
        .body(Body::empty())
        .expect("get-session request should build");
    let first_detail = response_json(
        app.clone().oneshot(get_first).await.unwrap(),
        StatusCode::OK,
        format!("GET /api/v1/sessions/{first_id}"),
    )
    .await;
    let after_first_created_seq = first_detail["last_seq"].as_u64().unwrap() + 1;
    delete_openai_credential(&app).await;

    let turn_id = fabro_types::TurnId::new();
    let submit = Request::builder()
        .method("POST")
        .uri(api(&format!("/sessions/{first_id}/turns")))
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"turn_id":"{turn_id}","input":"Which provider?"}}"#
        )))
        .expect("submit-turn request should build");
    let response = app.clone().oneshot(submit).await.unwrap();
    assert_eq!(
        response.headers().get("x-fabro-turn-id").unwrap(),
        turn_id.to_string().as_str()
    );
    let _ = session_sse_events(response).await;

    let request = Request::builder()
        .method("GET")
        .uri(api(&format!(
            "/sessions/{first_id}/events?since_seq={after_first_created_seq}&limit=1"
        )))
        .body(Body::empty())
        .expect("session events request should build");
    let page = response_json(
        app.clone().oneshot(request).await.unwrap(),
        StatusCode::OK,
        format!("GET /api/v1/sessions/{first_id}/events"),
    )
    .await;

    assert_eq!(page["data"].as_array().unwrap().len(), 1);
    assert_eq!(page["data"][0]["session_id"], first_id);
    assert_ne!(page["data"][0]["session_id"], second_id);
    assert_eq!(page["data"][0]["event"], "run.session.turn.started");
    assert_eq!(
        page["data"][0]["properties"]["turn_id"],
        turn_id.to_string()
    );
    assert_eq!(page["meta"]["has_more"], true);
}

#[tokio::test]
async fn inactive_turn_interrupt_returns_conflict() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let (run_id, _run_id_workspace) = create_run(&app).await;
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

async fn session_sse_events(response: axum::response::Response) -> Vec<serde_json::Value> {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("session event stream body should be readable");
    let body = String::from_utf8(bytes.to_vec()).expect("session event stream should be UTF-8");
    body.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|data| serde_json::from_str(data).expect("session event data should be JSON"))
        .collect()
}

async fn delete_openai_credential(app: &axum::Router) {
    let request = Request::builder()
        .method("DELETE")
        .uri(api("/secrets"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "name": EnvVars::OPENAI_API_KEY,
            }))
            .expect("delete-secret request should serialize"),
        ))
        .expect("delete-secret request should build");
    response_status(
        app.clone().oneshot(request).await.unwrap(),
        StatusCode::NO_CONTENT,
        "DELETE /api/v1/secrets",
    )
    .await;
}

fn assert_session_metadata_only(value: &serde_json::Value) {
    let object = value
        .as_object()
        .expect("session response should be a JSON object");
    assert_eq!(value["provider"], "openai");
    for field in [
        "working_dir",
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
