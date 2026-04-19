//! End-to-end HTTP coverage for the archived run status: the R14 mutation
//! rejection contract, archive/unarchive status codes, and the
//! `include_archived` listing filter.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, body_json, create_and_start_run_from_manifest,
    minimal_manifest_json_with_dry_run, test_app_state_with_options, test_app_with_scheduler,
    test_settings, wait_for_run_status,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archived_runs_reject_mutations_with_actionable_body() {
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id =
        create_and_start_run_from_manifest(&app, minimal_manifest_json_with_dry_run(MINIMAL_DOT))
            .await;
    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(status, "succeeded");

    // Archive the run.
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/archive")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response.into_body()).await;
    assert_eq!(body["status"], "archived");

    // Every mutation endpoint that guards against archived runs returns 409 with
    // an actionable "unarchive first" body.
    for path in &["/cancel", "/pause", "/unpause", "/start"] {
        let req = Request::builder()
            .method("POST")
            .uri(api(&format!("/runs/{run_id}{path}")))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "expected 409 for POST /runs/{{id}}{path} on archived run"
        );
        let body = body_json(response.into_body()).await;
        let detail = body["errors"][0]["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("is archived") && detail.contains("fabro unarchive"),
            "expected archived-rejection body on {path}, got: {body}"
        );
    }

    // Client-supplied lifecycle events on the archived run are rejected.
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/events")))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "ts": "2026-04-19T12:00:00.000Z",
                "run_id": run_id,
                "event": "agent.message",
                "properties": {}
            }))
            .unwrap(),
        ))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "expected 409 for POST /runs/{{id}}/events on archived run"
    );

    // Additional write surfaces that the Unit 4 audit guards but the scenario
    // loop doesn't cover. The `reject_if_archived` check runs before each
    // endpoint's state-specific lookups, so synthetic stage/question/filename
    // values are fine — the archive guard fires first.
    for (method, path, body, content_type) in [
        (
            "POST",
            format!("/runs/{run_id}/questions/q-fake/answer"),
            r#"{"value":"x"}"#,
            "application/json",
        ),
        (
            "POST",
            format!("/runs/{run_id}/stages/fake@1/artifacts?filename=smoke.txt"),
            "payload",
            "application/octet-stream",
        ),
        (
            "PUT",
            format!("/runs/{run_id}/sandbox/file?path=smoke.txt"),
            "payload",
            "application/octet-stream",
        ),
        (
            "POST",
            format!("/runs/{run_id}/blobs"),
            "payload",
            "application/octet-stream",
        ),
    ] {
        let req = Request::builder()
            .method(method)
            .uri(api(&path))
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "expected 409 for {method} {path} on archived run"
        );
        let body = body_json(response.into_body()).await;
        let detail = body["errors"][0]["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("is archived") && detail.contains("fabro unarchive"),
            "expected archived-rejection body on {method} {path}, got: {body}"
        );
    }

    // Unarchive restores the prior terminal status.
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/unarchive")))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response.into_body()).await;
    assert_eq!(body["status"], "succeeded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn appending_run_archived_event_directly_is_rejected() {
    // Regression: archive/unarchive events must not be injectable via
    // `append_run_event` — clients must use the operation endpoints.
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id =
        create_and_start_run_from_manifest(&app, minimal_manifest_json_with_dry_run(MINIMAL_DOT))
            .await;
    wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;

    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/events")))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "ts": "2026-04-19T12:00:00.000Z",
                "run_id": run_id,
                "event": "run.archived",
                "properties": {}
            }))
            .unwrap(),
        ))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response.into_body()).await;
    let detail = body["errors"][0]["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("lifecycle event"),
        "expected lifecycle rejection, got: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_returns_404_for_unknown_run() {
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs/01ARZ3NDEKTSV4RRFFQ69G5FAV/archive"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let req = Request::builder()
        .method("POST")
        .uri(api("/runs/01ARZ3NDEKTSV4RRFFQ69G5FAV/unarchive"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_runs_respects_include_archived_flag() {
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id =
        create_and_start_run_from_manifest(&app, minimal_manifest_json_with_dry_run(MINIMAL_DOT))
            .await;
    wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;

    // Archive it.
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/archive")))
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(req).await.unwrap();

    // Default listing hides archived.
    let req = Request::builder()
        .method("GET")
        .uri(api("/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let ids_visible: Vec<String> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["run_id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        !ids_visible.contains(&run_id),
        "archived run should be hidden, got {ids_visible:?}"
    );

    // `include_archived=true` surfaces it.
    let req = Request::builder()
        .method("GET")
        .uri(api("/runs?include_archived=true"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body = body_json(response.into_body()).await;
    let ids_all: Vec<String> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["run_id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        ids_all.contains(&run_id),
        "include_archived=true should surface the run, got {ids_all:?}"
    );
}
