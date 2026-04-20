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
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::build_router;
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, minimal_manifest_json, response_json, response_status, test_app_state,
};

fn files_url(run_id: &str) -> String {
    api(&format!("/runs/{run_id}/files"))
}

#[tokio::test]
async fn invalid_run_id_returns_400() {
    let app = build_router(test_app_state(), AuthMode::Disabled);
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
    let app = build_router(test_app_state(), AuthMode::Disabled);
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
    let app = build_router(test_app_state(), AuthMode::Disabled);
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
async fn non_default_from_sha_returns_400_even_when_hex() {
    let app = build_router(test_app_state(), AuthMode::Disabled);
    let fake = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    // Well-formed hex SHA but v1 reserves the parameter for a future
    // version; any non-default value must be rejected.
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
        format!("GET /api/v1/runs/{fake}/files?from_sha=<non-default>"),
    )
    .await;
}

#[tokio::test]
async fn malformed_to_sha_returns_400() {
    let app = build_router(test_app_state(), AuthMode::Disabled);
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
async fn submitted_run_without_sandbox_returns_empty_envelope() {
    // A run that has been created but not started has no base_sha or
    // sandbox record, so the handler returns an empty envelope. The UI
    // maps that to R4(a).
    let app = build_router(test_app_state(), AuthMode::Disabled);
    let manifest = minimal_manifest_json(MINIMAL_DOT);
    let create_req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&manifest).unwrap()))
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
    // Degraded is false because there's no final_patch either — the run
    // simply hasn't produced anything to diff.
    assert_eq!(body["meta"]["degraded"].as_bool(), Some(false));
}

#[tokio::test]
async fn demo_mode_returns_fixture_without_touching_store() {
    // R34: demo handler must return the illustrative fixture with no
    // cross-contamination with real run state.
    let app = build_router(test_app_state(), AuthMode::Disabled);
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
    let app = build_router(test_app_state(), AuthMode::Disabled);
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
    assert!(body["meta"]["truncated"].is_boolean());
    assert!(body["meta"]["total_changed"].is_number());
    for entry in body["data"].as_array().unwrap() {
        assert!(entry["old_file"]["name"].is_string());
        assert!(entry["old_file"]["contents"].is_string());
        assert!(entry["new_file"]["name"].is_string());
        assert!(entry["new_file"]["contents"].is_string());
    }
}
