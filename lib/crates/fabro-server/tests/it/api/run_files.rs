//! HTTP-level integration tests for `GET /api/v1/runs/{id}/files`.
//!
//! These tests exercise the handler's request-plumbing branches —
//! authentication extractor, route matching, query validation, demo-mode
//! branching, and the empty-envelope / not-found responses — without
//! requiring a reconnected sandbox. The sandbox happy path is covered by
//! unit tests on the sandbox-git helpers and by `stitch_file_diff` tests
//! in `run_files.rs`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_server::server::create_app_state_with_store;
use fabro_store::{ArtifactStore, Database};
use fabro_types::RunId;
use fabro_workflow::event as workflow_event;
use fabro_workflow::run_status::SuccessReason;
use object_store::memory::InMemory as MemoryObjectStore;
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, minimal_manifest_json, response_json, response_status, test_app_state,
    test_settings,
};

fn files_url(run_id: &str) -> String {
    api(&format!("/runs/{run_id}/files"))
}

fn store_bundle() -> (Arc<Database>, ArtifactStore) {
    let object_store: Arc<dyn object_store::ObjectStore> = Arc::new(MemoryObjectStore::new());
    let store = Arc::new(Database::new(
        Arc::clone(&object_store),
        "",
        Duration::from_millis(1),
        None,
    ));
    let artifact_store = ArtifactStore::new(object_store, "artifacts");
    (store, artifact_store)
}

async fn append_completed_run_with_final_patch(
    store: &Database,
    run_id: &RunId,
    final_patch: &str,
) {
    let run_store = store.create_run(run_id).await.expect("create run store");
    workflow_event::append_event(
        &run_store,
        run_id,
        &workflow_event::Event::WorkflowRunStarted {
            name:         "test".to_string(),
            run_id:       *run_id,
            base_branch:  Some("main".to_string()),
            base_sha:     Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
            run_branch:   Some("fabro/run/test".to_string()),
            worktree_dir: None,
            goal:         Some("Test degraded files".to_string()),
        },
    )
    .await
    .expect("append WorkflowRunStarted");
    workflow_event::append_event(
        &run_store,
        run_id,
        &workflow_event::Event::WorkflowRunCompleted {
            duration_ms:          1,
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()),
            final_patch:          Some(final_patch.to_string()),
            billing:              None,
        },
    )
    .await
    .expect("append WorkflowRunCompleted");
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
async fn non_default_from_sha_returns_400_even_when_hex() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
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
async fn submitted_run_without_sandbox_returns_empty_envelope() {
    // A run that has been created but not started has no base_sha or
    // sandbox record, so the handler returns an empty envelope. The UI
    // maps that to R4(a).
    let app = fabro_server::test_support::build_test_router(test_app_state());
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
async fn degraded_run_returns_file_diff_shape_without_meta_patch() {
    let settings = test_settings();
    let (store, artifact_store) = store_bundle();
    let state = create_app_state_with_store(
        settings.server_settings,
        settings.manifest_run_defaults,
        5,
        Arc::clone(&store),
        artifact_store,
    );
    let app = fabro_server::test_support::build_test_router(state);
    let run_id = RunId::new();
    let patch = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1,2 @@
 old
+new
diff --git a/.env.production b/.env.production
--- a/.env.production
+++ b/.env.production
@@ -1 +1 @@
-SECRET=old
+SECRET=new
";
    append_completed_run_with_final_patch(&store, &run_id, patch).await;

    let req = Request::builder()
        .method("GET")
        .uri(files_url(&run_id.to_string()))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let body = response_json(
        resp,
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/files"),
    )
    .await;

    assert_eq!(body["meta"]["degraded"].as_bool(), Some(true));
    assert!(body["meta"]["degraded_reason"].is_string());
    assert!(body["meta"].get("patch").is_none());
    assert_eq!(body["meta"]["total_changed"], 2);
    assert_eq!(body["meta"]["truncated"].as_bool(), Some(false));

    let data = body["data"].as_array().expect("data should be an array");
    assert_eq!(data.len(), 2);
    assert_eq!(data[0]["old_file"]["contents"], serde_json::Value::Null);
    assert_eq!(data[0]["new_file"]["contents"], serde_json::Value::Null);
    assert!(data[0]["unified_patch"].is_string());
    assert_eq!(data[1]["sensitive"].as_bool(), Some(true));
    assert_eq!(data[1]["old_file"]["contents"], serde_json::Value::Null);
    assert_eq!(data[1]["new_file"]["contents"], serde_json::Value::Null);
    assert!(data[1].get("unified_patch").is_none());
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
    assert!(body["meta"]["truncated"].is_boolean());
    assert!(body["meta"]["total_changed"].is_number());
    for entry in body["data"].as_array().unwrap() {
        assert!(entry["old_file"]["name"].is_string());
        assert!(entry["old_file"]["contents"].is_string());
        assert!(entry["new_file"]["name"].is_string());
        assert!(entry["new_file"]["contents"].is_string());
    }
}
