//! HTTP-level integration tests for `GET /api/v1/runs/{id}/files`.
//!
//! These tests exercise the handler's request-plumbing branches —
//! authentication extractor, route matching, query validation, demo-mode
//! branching, and the empty-envelope / not-found responses — without
//! requiring a reconnected sandbox. The sandbox happy path is covered by
//! unit tests on the sandbox-git helpers and by `stitch_file_diff` tests
//! in `run_files.rs`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, minimal_intent_json, response_json, response_status, test_app_state,
};

fn files_url(run_id: &str) -> String {
    api(&format!("/runs/{run_id}/files"))
}

fn commits_url(run_id: &str) -> String {
    api(&format!("/runs/{run_id}/commits"))
}

fn files_url_with_scope(run_id: &str, scope: &str) -> String {
    format!("{}?scope={scope}", files_url(run_id))
}

#[tokio::test]
async fn invalid_run_id_returns_400() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let req = Request::builder()
        .method("GET")
        .uri(files_url("not-a-ulid"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    response_status(
        resp,
        StatusCode::BAD_REQUEST,
        "GET /api/v1/runs/not-a-ulid/files",
    )
    .await;
}

#[tokio::test]
async fn unknown_run_returns_404() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    // Valid ULID format but not a run we've created.
    let fake = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let req = Request::builder()
        .method("GET")
        .uri(files_url(fake))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    response_status(
        resp,
        StatusCode::NOT_FOUND,
        format!("GET /api/v1/runs/{fake}/files"),
    )
    .await;
}

#[tokio::test]
async fn malformed_from_sha_query_returns_400() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let fake = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let req = Request::builder()
        .method("GET")
        .uri(format!("{}?from_sha=not-hex", files_url(fake)))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let body = crate::helpers::response_json(
        resp,
        StatusCode::BAD_REQUEST,
        format!("{}:{}", file!(), line!()),
    )
    .await;
    assert!(
        body["errors"][0]["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("from_sha")
    );
}

#[tokio::test]
async fn one_sided_from_sha_returns_400_even_when_hex() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let fake = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "{}?from_sha=abc1234def56789abc1234def56789abc1234def",
            files_url(fake)
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    response_status(
        resp,
        StatusCode::BAD_REQUEST,
        format!("GET /api/v1/runs/{fake}/files?from_sha=<one-sided>"),
    )
    .await;
}

#[tokio::test]
async fn malformed_to_sha_returns_400() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let fake = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let req = Request::builder()
        .method("GET")
        .uri(format!("{}?to_sha=xyz", files_url(fake)))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    response_status(
        resp,
        StatusCode::BAD_REQUEST,
        format!("GET /api/v1/runs/{fake}/files?to_sha=xyz"),
    )
    .await;
}

#[tokio::test]
async fn invalid_scope_returns_400() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let fake = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let req = Request::builder()
        .method("GET")
        .uri(files_url_with_scope(fake, "dirty"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    response_status(
        resp,
        StatusCode::BAD_REQUEST,
        format!("GET /api/v1/runs/{fake}/files?scope=dirty"),
    )
    .await;
}

#[tokio::test]
async fn submitted_run_without_sandbox_returns_empty_envelope() {
    let workspace = tempfile::tempdir().unwrap();
    // A run that has been created but not started has no base_sha or
    // run sandbox, so the handler returns an empty envelope. The UI
    // maps that to R4(a).
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let intent = minimal_intent_json(&app, MINIMAL_DOT, workspace.path()).await;
    let create_req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&intent).unwrap()))
        .unwrap();
    let create_resp = app.clone().oneshot(create_req).await.unwrap();
    let create_body = response_json(create_resp, StatusCode::CREATED, "POST /api/v1/runs").await;
    let run_id = create_body["id"].as_str().unwrap().to_string();

    let req = Request::builder()
        .method("GET")
        .uri(files_url(&run_id))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let body = response_json(
        resp,
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/files"),
    )
    .await;
    assert!(
        body["data"].as_array().is_some_and(Vec::is_empty),
        "expected empty data: {body}"
    );
    assert_eq!(body["meta"]["total_changed"], 0);
    assert_eq!(body["meta"]["source"].as_str(), Some("final_patch"));
    assert_eq!(body["meta"]["scope"].as_str(), Some("committed"));
    // Degraded is false because there's no final_patch either — the run
    // simply hasn't produced anything to diff.
    assert_eq!(body["meta"]["degraded"].as_bool(), Some(false));
}

#[tokio::test]
async fn demo_mode_returns_fixture_without_touching_store() {
    // R34: demo handler must return the illustrative fixture with no
    // cross-contamination with real run state.
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let arbitrary = "not-even-a-valid-ulid-for-run";

    let req = Request::builder()
        .method("GET")
        .uri(files_url(arbitrary))
        .header("x-fabro-demo", "1")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let body = response_json(resp, StatusCode::OK, "GET /api/v1/runs/whatever/files").await;

    // Demo fixture ships three entries (modified + added + renamed).
    assert_eq!(body["meta"]["source"].as_str(), Some("sandbox"));
    assert_eq!(body["meta"]["scope"].as_str(), Some("committed"));
    let data = body["data"].as_array().expect("data array");
    assert_eq!(data.len(), 3, "demo fixture should have 3 entries");
    // At least one entry must render with populated contents to prove the
    // fixture exercises the MultiFileDiff branch.
    let has_content = data.iter().any(|entry| {
        entry["new_file"]["contents"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    });
    assert!(has_content, "demo fixture should contain populated content");
}

#[tokio::test]
async fn response_envelope_matches_openapi_paginated_run_file_list_shape() {
    // Sanity check that the happy-path envelope shape matches what the
    // OpenAPI spec + regenerated TS client expect. Uses demo mode so the
    // test stays deterministic without running a sandbox.
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let req = Request::builder()
        .method("GET")
        .uri(files_url("whatever"))
        .header("x-fabro-demo", "1")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let body = response_json(resp, StatusCode::OK, "GET /api/v1/runs/whatever/files").await;

    assert!(body["data"].is_array());
    assert!(body["meta"].is_object());
    assert!(body.get("source").is_none());
    assert!(body["meta"]["source"].is_string());
    assert!(body["meta"]["scope"].is_string());
    assert!(body["meta"]["truncated"].is_boolean());
    assert!(body["meta"]["total_changed"].is_number());
    for entry in body["data"].as_array().unwrap() {
        assert!(entry["old_file"]["name"].is_string());
        assert!(entry["old_file"]["contents"].is_string());
        assert!(entry["new_file"]["name"].is_string());
        assert!(entry["new_file"]["contents"].is_string());
    }
}

#[tokio::test]
async fn commit_response_envelope_matches_openapi_paginated_run_commit_list_shape() {
    // Sanity check that the commits route is wired and returns the envelope
    // shape generated into the TypeScript client. Demo mode keeps this
    // route-level test deterministic without requiring a live sandbox.
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let req = Request::builder()
        .method("GET")
        .uri(commits_url("whatever"))
        .header("x-fabro-demo", "1")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let body = response_json(resp, StatusCode::OK, "GET /api/v1/runs/whatever/commits").await;

    assert!(body["data"].is_array());
    assert!(body["meta"].is_object());
    assert_eq!(body["meta"]["source"].as_str(), Some("sandbox"));
    assert!(body["meta"]["base_sha"].is_string());
    assert!(body["meta"]["head_sha"].is_string());
    assert!(body["meta"]["limit"].is_number());
    assert!(body["meta"]["total_returned"].is_number());
    assert!(body["meta"]["truncated"].is_boolean());

    let data = body["data"].as_array().expect("data array");
    assert_eq!(data.len(), 1, "demo commits fixture should have one commit");
    let commit = &data[0];
    assert!(commit["sha"].is_string());
    assert!(commit["short_sha"].is_string());
    assert!(commit["parents"].is_array());
    assert!(commit["author"].is_object());
    assert!(commit["committer"].is_object());
    assert!(commit["subject"].is_string());
    assert!(commit["message"].is_string());
    assert!(commit["trailers"].is_object());
    assert!(commit["tree_sha"].is_string());
}
