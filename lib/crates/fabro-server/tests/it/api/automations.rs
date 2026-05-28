use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use fabro_server::server::build_router;
use fabro_server::test_support::{
    TestAppStateBuilder, TestAutomationRunMaterializer, build_test_router, test_auth_mode,
};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, checked_response, minimal_manifest_json, response_json, response_status,
    run_json,
};

fn automation_body(id: &str, name: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "description": "Runs on a schedule.",
        "enabled": true,
        "target": {
            "repository": "fabro-sh/fabro",
            "ref": "main",
            "workflow": "release"
        },
        "triggers": [
            {
                "type": "api",
                "id": "manual",
                "enabled": true
            },
            {
                "type": "schedule",
                "id": "nightly",
                "enabled": true,
                "expression": "0 3 * * *"
            }
        ]
    })
}

fn replacement_body(name: &str) -> Value {
    json!({
        "name": name,
        "description": null,
        "enabled": false,
        "target": {
            "repository": "fabro-sh/fabro",
            "ref": "main",
            "workflow": "release"
        },
        "triggers": [
            {
                "type": "api",
                "id": "manual",
                "enabled": false
            }
        ]
    })
}

fn automation_app() -> (axum::Router, tempfile::TempDir, PathBuf) {
    let temp_dir = tempfile::tempdir().expect("automation test tempdir should be created");
    let active_config_path = temp_dir.path().join("settings.toml");
    let automation_dir = temp_dir.path().join("automations");
    let state = TestAppStateBuilder::new()
        .active_config_path(active_config_path)
        .build();
    (build_test_router(state), temp_dir, automation_dir)
}

fn automation_app_with_fake_materializer() -> (axum::Router, tempfile::TempDir, PathBuf) {
    let temp_dir = tempfile::tempdir().expect("automation test tempdir should be created");
    let active_config_path = temp_dir.path().join("settings.toml");
    let automation_dir = temp_dir.path().join("automations");
    let materialized_manifest: fabro_api::types::RunManifest =
        serde_json::from_value(minimal_manifest_json(MINIMAL_DOT))
            .expect("minimal run manifest fixture should deserialize");
    let submitted_manifest_bytes =
        serde_json::to_vec(&materialized_manifest).expect("minimal run manifest should serialize");
    let state = TestAppStateBuilder::new()
        .active_config_path(active_config_path)
        .automation_materializer(TestAutomationRunMaterializer::succeed(
            materialized_manifest,
            submitted_manifest_bytes,
        ))
        .build();
    (build_test_router(state), temp_dir, automation_dir)
}

fn json_request(method: Method, path: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(api(path))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&body).expect("automation fixture should serialize"),
        ))
        .expect("automation JSON request should build")
}

fn empty_request(method: Method, path: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(api(path))
        .body(Body::empty())
        .expect("automation request should build")
}

fn request_with_if_match(
    method: Method,
    path: &str,
    revision: &str,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(api(path))
        .header(header::IF_MATCH, revision);
    let body = match body {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&value).expect("automation fixture should serialize"))
        }
        None => Body::empty(),
    };
    builder
        .body(body)
        .expect("automation If-Match request should build")
}

async fn create_automation(app: &axum::Router, id: &str, name: &str) -> Value {
    create_automation_with_body(app, &automation_body(id, name)).await
}

async fn create_automation_with_body(app: &axum::Router, body: &Value) -> Value {
    let response = app
        .clone()
        .oneshot(json_request(Method::POST, "/automations", body))
        .await
        .expect("create automation should respond");
    response_json(response, StatusCode::CREATED, "POST /api/v1/automations").await
}

async fn create_automation_run(
    app: &axum::Router,
    automation_id: &str,
    expected: StatusCode,
) -> Value {
    let response = app
        .clone()
        .oneshot(empty_request(
            Method::POST,
            &format!("/automations/{automation_id}/runs"),
        ))
        .await
        .expect("create automation run should respond");
    response_json(
        response,
        expected,
        format!("POST /api/v1/automations/{automation_id}/runs"),
    )
    .await
}

async fn list_automation_runs(app: &axum::Router, path: &str) -> Value {
    let response = app
        .clone()
        .oneshot(empty_request(Method::GET, path))
        .await
        .expect("list automation runs should respond");
    response_json(response, StatusCode::OK, format!("GET /api/v1{path}")).await
}

fn revision_from(body: &Value) -> &str {
    body["revision"]
        .as_str()
        .expect("automation response should include a revision")
}

#[tokio::test]
async fn empty_automation_list_returns_total_zero() {
    let (app, _temp_dir, _automation_dir) = automation_app();

    let response = app
        .oneshot(empty_request(Method::GET, "/automations"))
        .await
        .expect("list automations should respond");
    let body = response_json(response, StatusCode::OK, "GET /api/v1/automations").await;

    assert_eq!(
        body,
        json!({
            "data": [],
            "meta": {
                "total": 0
            }
        })
    );
}

#[tokio::test]
async fn create_automation_persists_sibling_toml_file() {
    let (app, _temp_dir, automation_dir) = automation_app();

    let body = create_automation(&app, "nightly", "Nightly").await;

    assert_eq!(body["id"], "nightly");
    assert_eq!(body["name"], "Nightly");
    assert!(automation_dir.join("nightly.toml").exists());
}

#[tokio::test]
async fn list_automations_returns_items_sorted_by_id() {
    let (app, _temp_dir, _automation_dir) = automation_app();
    create_automation(&app, "zulu", "Zulu").await;
    create_automation(&app, "alpha", "Alpha").await;

    let response = app
        .oneshot(empty_request(Method::GET, "/automations"))
        .await
        .expect("list automations should respond");
    let body = response_json(response, StatusCode::OK, "GET /api/v1/automations").await;

    assert_eq!(body["meta"]["total"], 2);
    assert_eq!(body["data"][0]["id"], "alpha");
    assert_eq!(body["data"][1]["id"], "zulu");
}

#[tokio::test]
async fn duplicate_automation_create_returns_conflict() {
    let (app, _temp_dir, _automation_dir) = automation_app();
    create_automation(&app, "nightly", "Nightly").await;

    let response = app
        .oneshot(json_request(
            Method::POST,
            "/automations",
            &automation_body("nightly", "Duplicate"),
        ))
        .await
        .expect("duplicate create should respond");

    response_status(
        response,
        StatusCode::CONFLICT,
        "POST /api/v1/automations duplicate",
    )
    .await;
}

#[tokio::test]
async fn get_automation_returns_current_etag() {
    let (app, _temp_dir, _automation_dir) = automation_app();
    let created = create_automation(&app, "nightly", "Nightly").await;
    let revision = revision_from(&created);

    let response = app
        .oneshot(empty_request(Method::GET, "/automations/nightly"))
        .await
        .expect("get automation should respond");
    let response =
        checked_response(response, StatusCode::OK, "GET /api/v1/automations/nightly").await;

    assert_eq!(
        response
            .headers()
            .get(header::ETAG)
            .expect("GET automation should include ETag"),
        &format!("\"{revision}\"")
    );
    let body = crate::helpers::body_json(response.into_body()).await;
    assert_eq!(body["revision"], revision);
}

#[tokio::test]
async fn replace_automation_accepts_unquoted_if_match_and_returns_new_etag() {
    let (app, _temp_dir, _automation_dir) = automation_app();
    let created = create_automation(&app, "nightly", "Nightly").await;
    let revision = revision_from(&created);

    let response = app
        .oneshot(request_with_if_match(
            Method::PUT,
            "/automations/nightly",
            revision,
            Some(replacement_body("Updated")),
        ))
        .await
        .expect("replace automation should respond");
    let response =
        checked_response(response, StatusCode::OK, "PUT /api/v1/automations/nightly").await;
    let etag = response
        .headers()
        .get(header::ETAG)
        .expect("PUT automation should include ETag")
        .to_str()
        .expect("ETag should be ASCII")
        .to_string();
    let body = crate::helpers::body_json(response.into_body()).await;

    assert_eq!(body["name"], "Updated");
    assert_ne!(body["revision"], revision);
    assert_eq!(etag, format!("\"{}\"", revision_from(&body)));
}

#[tokio::test]
async fn stale_automation_replace_returns_conflict() {
    let (app, _temp_dir, _automation_dir) = automation_app();
    let created = create_automation(&app, "nightly", "Nightly").await;
    let stale_revision = revision_from(&created).to_string();

    let replaced = app
        .clone()
        .oneshot(request_with_if_match(
            Method::PUT,
            "/automations/nightly",
            &stale_revision,
            Some(replacement_body("Updated")),
        ))
        .await
        .expect("first replace should respond");
    response_status(
        replaced,
        StatusCode::OK,
        "PUT /api/v1/automations/nightly first replace",
    )
    .await;

    let response = app
        .oneshot(request_with_if_match(
            Method::PUT,
            "/automations/nightly",
            &stale_revision,
            Some(replacement_body("Stale")),
        ))
        .await
        .expect("stale replace should respond");

    response_status(
        response,
        StatusCode::CONFLICT,
        "PUT /api/v1/automations/nightly stale",
    )
    .await;
}

#[tokio::test]
async fn replace_and_delete_automation_require_if_match() {
    let (app, _temp_dir, _automation_dir) = automation_app();
    create_automation(&app, "nightly", "Nightly").await;

    let replace_response = app
        .clone()
        .oneshot(json_request(
            Method::PUT,
            "/automations/nightly",
            &replacement_body("Updated"),
        ))
        .await
        .expect("replace without If-Match should respond");
    response_status(
        replace_response,
        StatusCode::PRECONDITION_REQUIRED,
        "PUT /api/v1/automations/nightly without If-Match",
    )
    .await;

    let delete_response = app
        .oneshot(empty_request(Method::DELETE, "/automations/nightly"))
        .await
        .expect("delete without If-Match should respond");
    response_status(
        delete_response,
        StatusCode::PRECONDITION_REQUIRED,
        "DELETE /api/v1/automations/nightly without If-Match",
    )
    .await;
}

#[tokio::test]
async fn delete_automation_removes_file_and_resource() {
    let (app, _temp_dir, automation_dir) = automation_app();
    let created = create_automation(&app, "nightly", "Nightly").await;
    let revision = revision_from(&created);

    let response = app
        .clone()
        .oneshot(request_with_if_match(
            Method::DELETE,
            "/automations/nightly",
            &format!("\"{revision}\""),
            None,
        ))
        .await
        .expect("delete automation should respond");
    response_status(
        response,
        StatusCode::NO_CONTENT,
        "DELETE /api/v1/automations/nightly",
    )
    .await;

    assert!(!automation_dir.join("nightly.toml").exists());
    let response = app
        .oneshot(empty_request(Method::GET, "/automations/nightly"))
        .await
        .expect("get deleted automation should respond");
    response_status(
        response,
        StatusCode::NOT_FOUND,
        "GET /api/v1/automations/nightly after delete",
    )
    .await;
}

#[tokio::test]
async fn invalid_trigger_ids_are_unprocessable() {
    let (app, _temp_dir, _automation_dir) = automation_app();
    let mut body = automation_body("nightly", "Nightly");
    body["triggers"][0]["id"] = json!("Bad!");

    let response = app
        .oneshot(json_request(Method::POST, "/automations", &body))
        .await
        .expect("invalid trigger id create should respond");

    response_status(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /api/v1/automations invalid trigger id",
    )
    .await;
}

#[tokio::test]
async fn empty_automation_name_is_unprocessable() {
    let (app, _temp_dir, _automation_dir) = automation_app();
    let mut body = automation_body("nightly", "Nightly");
    body["name"] = json!(" ");

    let response = app
        .oneshot(json_request(Method::POST, "/automations", &body))
        .await
        .expect("empty automation name create should respond");

    response_status(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /api/v1/automations empty name",
    )
    .await;
}

#[tokio::test]
async fn duplicate_trigger_ids_are_unprocessable() {
    let (app, _temp_dir, _automation_dir) = automation_app();
    let mut body = automation_body("nightly", "Nightly");
    body["triggers"][1]["id"] = json!("manual");

    let response = app
        .oneshot(json_request(Method::POST, "/automations", &body))
        .await
        .expect("duplicate trigger create should respond");

    response_status(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /api/v1/automations duplicate trigger ids",
    )
    .await;
}

#[tokio::test]
async fn second_api_trigger_is_unprocessable() {
    let (app, _temp_dir, _automation_dir) = automation_app();
    let mut body = automation_body("nightly", "Nightly");
    body["triggers"][1] = json!({
        "type": "api",
        "id": "manual2",
        "enabled": true
    });

    let response = app
        .oneshot(json_request(Method::POST, "/automations", &body))
        .await
        .expect("second API trigger create should respond");

    response_status(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /api/v1/automations second API trigger",
    )
    .await;
}

#[tokio::test]
async fn invalid_schedule_expression_is_unprocessable() {
    let (app, _temp_dir, _automation_dir) = automation_app();
    let mut body = automation_body("nightly", "Nightly");
    body["triggers"][1]["expression"] = json!("60 3 * * *");

    let response = app
        .oneshot(json_request(Method::POST, "/automations", &body))
        .await
        .expect("invalid schedule create should respond");

    response_status(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /api/v1/automations invalid schedule",
    )
    .await;
}

#[tokio::test]
async fn automation_store_malformed_persisted_toml_fails_startup() {
    let temp_dir = tempfile::tempdir().expect("automation test tempdir should be created");
    let automation_dir = temp_dir.path().join("automations");
    tokio::fs::create_dir_all(&automation_dir)
        .await
        .expect("automation dir should be created");
    tokio::fs::write(automation_dir.join("broken.toml"), "not valid toml =")
        .await
        .expect("broken automation fixture should be written");

    let result = TestAppStateBuilder::new()
        .active_config_path(temp_dir.path().join("settings.toml"))
        .try_build();

    assert!(result.is_err());
}

#[tokio::test]
async fn automations_routes_require_authenticated_user() {
    let temp_dir = tempfile::tempdir().expect("automation test tempdir should be created");
    let state = TestAppStateBuilder::new()
        .active_config_path(temp_dir.path().join("settings.toml"))
        .build();
    let app = build_router(state, test_auth_mode());

    let response = app
        .oneshot(empty_request(Method::GET, "/automations"))
        .await
        .expect("unauthenticated automation list should respond");

    response_status(
        response,
        StatusCode::UNAUTHORIZED,
        "GET /api/v1/automations without auth",
    )
    .await;
}

#[tokio::test]
async fn disabled_automation_run_endpoint_returns_conflict_code() {
    let (app, _temp_dir, _automation_dir) = automation_app_with_fake_materializer();
    let mut body = automation_body("nightly", "Nightly");
    body["enabled"] = json!(false);
    create_automation_with_body(&app, &body).await;

    let error = create_automation_run(&app, "nightly", StatusCode::CONFLICT).await;

    assert_eq!(
        error["errors"][0]["code"],
        "automation_api_trigger_disabled"
    );
}

#[tokio::test]
async fn missing_automation_run_endpoint_returns_not_found() {
    let (app, _temp_dir, _automation_dir) = automation_app_with_fake_materializer();

    create_automation_run(&app, "missing", StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn disabled_api_trigger_run_endpoint_returns_conflict_code() {
    let (app, _temp_dir, _automation_dir) = automation_app_with_fake_materializer();
    let mut body = automation_body("nightly", "Nightly");
    body["triggers"][0]["enabled"] = json!(false);
    create_automation_with_body(&app, &body).await;

    let error = create_automation_run(&app, "nightly", StatusCode::CONFLICT).await;

    assert_eq!(
        error["errors"][0]["code"],
        "automation_api_trigger_disabled"
    );
}

#[tokio::test]
async fn missing_api_trigger_run_endpoint_returns_conflict_code() {
    let (app, _temp_dir, _automation_dir) = automation_app_with_fake_materializer();
    let mut body = automation_body("nightly", "Nightly");
    body["triggers"] = json!([
        {
            "type": "schedule",
            "id": "nightly",
            "enabled": true,
            "expression": "0 3 * * *"
        }
    ]);
    create_automation_with_body(&app, &body).await;

    let error = create_automation_run(&app, "nightly", StatusCode::CONFLICT).await;

    assert_eq!(
        error["errors"][0]["code"],
        "automation_api_trigger_disabled"
    );
}

#[tokio::test]
async fn successful_api_triggered_automation_run_persists_automation_metadata() {
    let (app, _temp_dir, _automation_dir) = automation_app_with_fake_materializer();
    create_automation(&app, "nightly", "Nightly").await;

    let created = create_automation_run(&app, "nightly", StatusCode::CREATED).await;

    assert_eq!(created["automation"]["id"], "nightly");
    assert_eq!(created["automation"]["name"], "Nightly");
    assert_eq!(created["automation"]["trigger_id"], "manual");

    let run_id = created["id"]
        .as_str()
        .expect("created automation run should include id");
    let retrieved = run_json(&app, run_id).await;
    assert_eq!(retrieved["id"], run_id);
    assert_eq!(retrieved["automation"], created["automation"]);
}

#[tokio::test]
async fn automation_run_listing_includes_only_runs_for_that_automation() {
    let (app, _temp_dir, _automation_dir) = automation_app_with_fake_materializer();
    create_automation(&app, "nightly", "Nightly").await;
    create_automation(&app, "weekly", "Weekly").await;
    let nightly = create_automation_run(&app, "nightly", StatusCode::CREATED).await;
    let weekly = create_automation_run(&app, "weekly", StatusCode::CREATED).await;

    let body = list_automation_runs(&app, "/automations/nightly/runs").await;

    assert_eq!(body["meta"]["total"], 1);
    assert_eq!(body["meta"]["has_more"], false);
    assert_eq!(
        body["data"].as_array().expect("data should be array").len(),
        1
    );
    assert_eq!(body["data"][0]["id"], nightly["id"]);
    assert_ne!(body["data"][0]["id"], weekly["id"]);
    assert_eq!(body["data"][0]["automation"]["id"], "nightly");
}

#[tokio::test]
async fn missing_automation_run_listing_returns_not_found() {
    let (app, _temp_dir, _automation_dir) = automation_app_with_fake_materializer();

    let response = app
        .oneshot(empty_request(Method::GET, "/automations/missing/runs"))
        .await
        .expect("missing automation run listing should respond");

    response_status(
        response,
        StatusCode::NOT_FOUND,
        "GET /api/v1/automations/missing/runs",
    )
    .await;
}

#[tokio::test]
async fn automation_run_listing_is_newest_first_and_paginates() {
    let (app, _temp_dir, _automation_dir) = automation_app_with_fake_materializer();
    create_automation(&app, "nightly", "Nightly").await;
    let oldest = create_automation_run(&app, "nightly", StatusCode::CREATED).await;
    let middle = create_automation_run(&app, "nightly", StatusCode::CREATED).await;
    let newest = create_automation_run(&app, "nightly", StatusCode::CREATED).await;

    let first_page = list_automation_runs(&app, "/automations/nightly/runs?page[limit]=2").await;
    assert_eq!(first_page["meta"]["total"], 3);
    assert_eq!(first_page["meta"]["has_more"], true);
    assert_eq!(first_page["data"][0]["id"], newest["id"]);
    assert_eq!(first_page["data"][1]["id"], middle["id"]);

    let second_page = list_automation_runs(
        &app,
        "/automations/nightly/runs?page[limit]=2&page[offset]=2",
    )
    .await;
    assert_eq!(second_page["meta"]["total"], 3);
    assert_eq!(second_page["meta"]["has_more"], false);
    assert_eq!(second_page["data"][0]["id"], oldest["id"]);
}
