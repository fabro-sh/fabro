use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use fabro_automation::{AutomationGitWorkflowSource, AutomationId, JiraIssueRunStore};
use fabro_config::Storage;
use fabro_server::server::build_router;
use fabro_server::test_support::{
    TestAppStateBuilder, TestAutomationRunMaterializer, build_test_router, test_auth_mode,
};
use fabro_static::EnvVars;
use fabro_types::{GitRunTarget, RunId};
use serde_json::{Value, json};
use sqlx::Row as _;
use tower::ServiceExt;

use crate::helpers::{api, checked_response, response_json, response_status, run_json};

fn automation_body(id: &str, name: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "description": "Runs on a schedule.",
        "environment_id": "default",
        "target": {
            "kind": "git",
            "repo": "fabro-sh/fabro",
            "branch": "main"
        },
        "workflow": "release",
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
        "environment_id": "default",
        "target": {
            "kind": "git",
            "repo": "fabro-sh/fabro",
            "branch": "main"
        },
        "workflow": "release",
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
    let vault_path = temp_dir.path().join("secrets.json");
    let sqlite_path = Storage::new(temp_dir.path()).sqlite_path();
    let state = TestAppStateBuilder::new()
        .active_config_path(active_config_path)
        .vault_path(vault_path)
        .build();
    (build_test_router(state), temp_dir, sqlite_path)
}

fn automation_app_with_fake_materializer() -> (axum::Router, tempfile::TempDir, PathBuf) {
    automation_app_with_materializer(TestAutomationRunMaterializer::succeed(GitRunTarget {
        repo:   "fabro-sh/fabro".to_string(),
        branch: "main".to_string(),
        tag:    None,
        sha:    Some("0123456789abcdef0123456789abcdef01234567".to_string()),
    }))
}

fn automation_app_with_materializer(
    materializer: TestAutomationRunMaterializer,
) -> (axum::Router, tempfile::TempDir, PathBuf) {
    let temp_dir = tempfile::tempdir().expect("automation test tempdir should be created");
    let active_config_path = temp_dir.path().join("settings.toml");
    let vault_path = temp_dir.path().join("secrets.json");
    let sqlite_path = Storage::new(temp_dir.path()).sqlite_path();
    let state = TestAppStateBuilder::new()
        .active_config_path(active_config_path)
        .vault_path(vault_path)
        .vault_entries([(EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
        .automation_materializer(materializer)
        .build();
    (build_test_router(state), temp_dir, sqlite_path)
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

fn assert_schedule_trigger(body: &Value, expression: &str, enabled: bool) {
    let trigger = body["triggers"]
        .as_array()
        .expect("automation response should include triggers")
        .iter()
        .find(|trigger| trigger["id"] == "nightly")
        .expect("automation response should include nightly schedule trigger");

    assert_eq!(
        trigger,
        &json!({
            "type": "schedule",
            "id": "nightly",
            "enabled": enabled,
            "expression": expression
        })
    );
}

async fn persisted_automation(sqlite_path: &Path, id: &str) -> Option<Value> {
    let database = fabro_db::Database::connect(sqlite_path)
        .await
        .expect("automation test database should open");
    let parent = sqlx::query("SELECT api_enabled FROM automations WHERE id = ?")
        .bind(id)
        .fetch_optional(database.pool())
        .await
        .expect("persisted automation should query")?;
    let triggers = sqlx::query(
        "SELECT id, enabled, expression FROM automation_triggers \
         WHERE automation_id = ? ORDER BY id",
    )
    .bind(id)
    .fetch_all(database.pool())
    .await
    .expect("persisted automation triggers should query")
    .into_iter()
    .map(|row| {
        json!({
            "id": row.get::<String, _>("id"),
            "enabled": row.get::<bool, _>("enabled"),
            "expression": row.get::<String, _>("expression")
        })
    })
    .collect::<Vec<_>>();
    Some(json!({
        "api_enabled": parent.get::<bool, _>("api_enabled"),
        "triggers": triggers
    }))
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
async fn create_automation_persists_sql_aggregate() {
    let (app, _temp_dir, sqlite_path) = automation_app();

    let body = create_automation(&app, "nightly", "Nightly").await;

    assert_eq!(body["id"], "nightly");
    assert_eq!(body["name"], "Nightly");
    assert_eq!(body["environment_id"], "default");
    assert_eq!(
        persisted_automation(&sqlite_path, "nightly").await,
        Some(json!({
            "api_enabled": true,
            "triggers": [{
                "id": "nightly",
                "enabled": true,
                "expression": "0 3 * * *"
            }]
        }))
    );
}

#[tokio::test]
async fn create_automation_requires_an_environment() {
    let (app, _temp_dir, _sqlite_path) = automation_app();
    let mut body = automation_body("nightly", "Nightly");
    body.as_object_mut()
        .expect("automation body should be an object")
        .remove("environment_id");

    let response = app
        .oneshot(json_request(Method::POST, "/automations", &body))
        .await
        .expect("create automation without environment should respond");
    let error = response_json(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /api/v1/automations without environment",
    )
    .await;

    assert_eq!(
        error["errors"][0]["code"],
        "automation_environment_required"
    );
}

#[tokio::test]
async fn create_automation_rejects_missing_and_local_environments() {
    let (app, _temp_dir, _sqlite_path) = automation_app();

    for (environment_id, expected_code) in [
        ("missing", "automation_environment_not_found"),
        ("local", "automation_environment_incompatible"),
    ] {
        let mut body = automation_body(environment_id, "Nightly");
        body["environment_id"] = json!(environment_id);
        let response = app
            .clone()
            .oneshot(json_request(Method::POST, "/automations", &body))
            .await
            .expect("invalid automation environment should respond");
        let error = response_json(
            response,
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("POST /api/v1/automations with {environment_id} environment"),
        )
        .await;

        assert_eq!(error["errors"][0]["code"], expected_code);
    }
}

#[tokio::test]
async fn automation_persists_across_app_rebuild() {
    let temp_dir = tempfile::tempdir().expect("automation test tempdir should be created");
    let active_config_path = temp_dir.path().join("settings.toml");
    let vault_path = temp_dir.path().join("secrets.json");
    let state = TestAppStateBuilder::new()
        .active_config_path(active_config_path.clone())
        .vault_path(vault_path.clone())
        .build();
    let app = build_test_router(state);
    let created = create_automation(&app, "nightly", "Nightly").await;
    drop(app);

    let state = TestAppStateBuilder::new()
        .active_config_path(active_config_path)
        .vault_path(vault_path)
        .build();
    let response = build_test_router(state)
        .oneshot(empty_request(Method::GET, "/automations/nightly"))
        .await
        .expect("get persisted automation should respond");
    let retrieved = response_json(
        response,
        StatusCode::OK,
        "GET /api/v1/automations/nightly after app rebuild",
    )
    .await;

    assert_eq!(retrieved, created);
}

#[tokio::test]
async fn schedule_trigger_round_trips_through_create_list_get_and_sql() {
    let (app, _temp_dir, sqlite_path) = automation_app();

    let created = create_automation(&app, "nightly", "Nightly").await;
    assert_schedule_trigger(&created, "0 3 * * *", true);

    let response = app
        .clone()
        .oneshot(empty_request(Method::GET, "/automations/nightly"))
        .await
        .expect("get automation should respond");
    let retrieved =
        response_json(response, StatusCode::OK, "GET /api/v1/automations/nightly").await;
    assert_schedule_trigger(&retrieved, "0 3 * * *", true);

    let response = app
        .oneshot(empty_request(Method::GET, "/automations"))
        .await
        .expect("list automations should respond");
    let list = response_json(response, StatusCode::OK, "GET /api/v1/automations").await;
    assert_schedule_trigger(&list["data"][0], "0 3 * * *", true);

    assert_eq!(
        persisted_automation(&sqlite_path, "nightly")
            .await
            .expect("automation should be persisted")["triggers"][0],
        json!({
            "id": "nightly",
            "enabled": true,
            "expression": "0 3 * * *"
        })
    );
}

#[tokio::test]
async fn invalid_stored_automation_returns_internal_server_error() {
    let (app, _temp_dir, sqlite_path) = automation_app();
    create_automation(&app, "nightly", "Nightly").await;
    let database = fabro_db::Database::connect(sqlite_path)
        .await
        .expect("automation test database should open");
    sqlx::query(
        "UPDATE automation_triggers SET expression = 'not cron' WHERE automation_id = 'nightly'",
    )
    .execute(database.pool())
    .await
    .expect("stored schedule should be corrupted for the test");

    let response = app
        .oneshot(empty_request(Method::GET, "/automations/nightly"))
        .await
        .expect("get invalid stored automation should respond");

    response_status(
        response,
        StatusCode::INTERNAL_SERVER_ERROR,
        "GET /api/v1/automations/nightly with invalid stored schedule",
    )
    .await;
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
async fn replace_automation_round_trips_schedule_trigger() {
    let (app, _temp_dir, sqlite_path) = automation_app();
    let created = create_automation(&app, "nightly", "Nightly").await;
    let revision = revision_from(&created);
    let mut replacement = replacement_body("Rescheduled");
    replacement["triggers"] = json!([
        {
            "type": "api",
            "id": "manual",
            "enabled": true
        },
        {
            "type": "schedule",
            "id": "nightly",
            "enabled": false,
            "expression": "30 4 * * *"
        }
    ]);

    let response = app
        .oneshot(request_with_if_match(
            Method::PUT,
            "/automations/nightly",
            revision,
            Some(replacement),
        ))
        .await
        .expect("replace automation should respond");
    let body = response_json(response, StatusCode::OK, "PUT /api/v1/automations/nightly").await;

    assert_schedule_trigger(&body, "30 4 * * *", false);
    assert_eq!(
        persisted_automation(&sqlite_path, "nightly")
            .await
            .expect("automation should be persisted")["triggers"][0],
        json!({
            "id": "nightly",
            "enabled": false,
            "expression": "30 4 * * *"
        })
    );
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
async fn delete_automation_removes_sql_aggregate() {
    let (app, _temp_dir, sqlite_path) = automation_app();
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

    assert!(
        persisted_automation(&sqlite_path, "nightly")
            .await
            .is_none()
    );
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
async fn unknown_trigger_type_is_unprocessable() {
    let (app, _temp_dir, _automation_dir) = automation_app();
    let mut body = automation_body("nightly", "Nightly");
    body["triggers"][1] = json!({
        "type": "event",
        "id": "event_trigger",
        "enabled": true
    });

    let response = app
        .oneshot(json_request(Method::POST, "/automations", &body))
        .await
        .expect("unknown trigger type create should respond");

    response_status(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /api/v1/automations unknown trigger type",
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
async fn legacy_automation_is_imported_and_directory_is_backed_up() {
    let temp_dir = tempfile::tempdir().expect("automation test tempdir should be created");
    let automation_dir = temp_dir.path().join("automations");
    tokio::fs::create_dir_all(&automation_dir)
        .await
        .expect("automation dir should be created");
    tokio::fs::write(
        automation_dir.join("nightly.toml"),
        r#"name = "Legacy nightly"

[target]
repository = "fabro-sh/fabro"
ref = "main"
workflow = "release"

[[triggers]]
type = "api"
id = "manual"
enabled = true
"#,
    )
    .await
    .expect("legacy automation fixture should be written");
    let state = TestAppStateBuilder::new()
        .active_config_path(temp_dir.path().join("settings.toml"))
        .vault_path(temp_dir.path().join("secrets.json"))
        .build();

    let response = build_test_router(state)
        .oneshot(empty_request(Method::GET, "/automations/nightly"))
        .await
        .expect("get imported automation should respond");
    let body = response_json(
        response,
        StatusCode::OK,
        "GET /api/v1/automations/nightly after legacy import",
    )
    .await;

    assert_eq!(body["name"], "Legacy nightly");
    assert!(!automation_dir.exists());
    let mut entries = tokio::fs::read_dir(temp_dir.path())
        .await
        .expect("storage directory should be readable");
    let mut backup_exists = false;
    while let Some(entry) = entries
        .next_entry()
        .await
        .expect("storage directory entry should be readable")
    {
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("automations.imported-")
        {
            backup_exists = true;
            break;
        }
    }
    assert!(backup_exists);
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
async fn api_triggered_automation_uses_its_selected_environment() {
    let (app, _temp_dir, _sqlite_path) = automation_app_with_fake_materializer();
    let environment = json!({
        "id": "smoke",
        "provider": "docker",
        "cwd": null,
        "image": { "docker": "buildpack-deps:noble", "dockerfile": null },
        "resources": { "cpu": 2, "memory": "4GB", "disk": null },
        "network": { "mode": "allow_all", "allow": [] },
        "lifecycle": { "preserve": false, "stop_on_terminal": true, "auto_stop": null },
        "labels": {},
        "env": {}
    });
    let response = app
        .clone()
        .oneshot(json_request(Method::POST, "/environments", &environment))
        .await
        .expect("create smoke environment should respond");
    response_status(
        response,
        StatusCode::CREATED,
        "POST /api/v1/environments smoke",
    )
    .await;
    let mut body = automation_body("nightly", "Nightly");
    body["environment_id"] = json!("smoke");
    create_automation_with_body(&app, &body).await;

    let created = create_automation_run(&app, "nightly", StatusCode::CREATED).await;
    let run_id = created["id"]
        .as_str()
        .expect("created automation run should include id");
    let response = app
        .oneshot(empty_request(
            Method::GET,
            &format!("/runs/{run_id}/settings"),
        ))
        .await
        .expect("automation run settings should respond");
    let settings = response_json(response, StatusCode::OK, "GET /api/v1/runs/{id}/settings").await;

    assert_eq!(settings["run"]["environment"]["id"], "smoke");
}

#[tokio::test]
async fn incomplete_legacy_automation_fails_before_materialization() {
    let materializer = TestAutomationRunMaterializer::fail_invalid_target();
    let (app, _temp_dir, sqlite_path) = automation_app_with_materializer(materializer);
    create_automation(&app, "nightly", "Nightly").await;
    let database = fabro_db::Database::connect(sqlite_path)
        .await
        .expect("automation test database should open");
    sqlx::query("UPDATE automations SET environment_id = NULL WHERE id = 'nightly'")
        .execute(database.pool())
        .await
        .expect("test automation environment should clear");

    let error = create_automation_run(&app, "nightly", StatusCode::CONFLICT).await;

    assert_eq!(
        error["errors"][0]["code"],
        "automation_environment_required"
    );
}

#[tokio::test]
async fn api_triggered_run_passes_saved_workflow_source_to_materialization() {
    let materializer = TestAutomationRunMaterializer::succeed(GitRunTarget {
        repo:   "fabro-sh/fabro".to_string(),
        branch: "main".to_string(),
        tag:    None,
        sha:    Some("0123456789abcdef0123456789abcdef01234567".to_string()),
    });
    let (app, _temp_dir, _automation_dir) = automation_app_with_materializer(materializer.clone());
    let mut body = automation_body("nightly", "Nightly");
    body["workflow_source"] = json!({
        "repo": "fabro-sh/workflows",
        "branch": "main",
        "tag": "release-v1"
    });
    create_automation_with_body(&app, &body).await;

    let created = create_automation_run(&app, "nightly", StatusCode::CREATED).await;

    let captured = materializer.captured_workflow_sources();
    assert_eq!(captured.len(), 1);
    assert_eq!(
        captured[0],
        Some(AutomationGitWorkflowSource {
            repo:   "fabro-sh/workflows".to_string(),
            branch: "main".to_string(),
            tag:    Some("release-v1".to_string()),
            sha:    None,
        })
    );
    assert_eq!(
        created["automation"]["workflow_source"],
        json!({
            "repo": "fabro-sh/workflows",
            "branch": "main",
            "tag": "release-v1",
            "resolved_sha": "ffffffffffffffffffffffffffffffffffffffff"
        })
    );
}

#[tokio::test]
async fn api_triggered_automation_with_missing_version_does_not_create_or_start_a_run() {
    let materializer = TestAutomationRunMaterializer::return_unstored_version(GitRunTarget {
        repo:   "fabro-sh/fabro".to_string(),
        branch: "main".to_string(),
        tag:    None,
        sha:    Some("0123456789abcdef0123456789abcdef01234567".to_string()),
    });
    let (app, _temp_dir, _automation_dir) = automation_app_with_materializer(materializer);
    create_automation(&app, "nightly", "Nightly").await;

    let error = create_automation_run(&app, "nightly", StatusCode::NOT_FOUND).await;

    assert_eq!(error["errors"][0]["code"], "workflow_version_not_found");
    let runs = list_automation_runs(&app, "/automations/nightly/runs").await;
    assert_eq!(runs["meta"]["total"], 0);
    assert_eq!(runs["data"], json!([]));
}

#[tokio::test]
async fn api_workflow_source_failure_does_not_create_or_start_a_run() {
    let materializer = TestAutomationRunMaterializer::fail_invalid_workflow_source();
    let (app, _temp_dir, _automation_dir) = automation_app_with_materializer(materializer);
    let mut body = automation_body("nightly", "Nightly");
    body["workflow_source"] = json!({
        "repo": "fabro-sh/workflows",
        "branch": "main"
    });
    create_automation_with_body(&app, &body).await;

    let error = create_automation_run(&app, "nightly", StatusCode::UNPROCESSABLE_ENTITY).await;

    assert!(
        error["errors"][0]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("workflow source"))
    );
    let runs = list_automation_runs(&app, "/automations/nightly/runs").await;
    assert_eq!(runs["meta"]["total"], 0);
    assert_eq!(runs["data"], json!([]));
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

async fn jira_claim_store(sqlite_path: &Path) -> JiraIssueRunStore {
    let database = fabro_db::Database::connect(sqlite_path)
        .await
        .expect("automation test database should open");
    JiraIssueRunStore::new(database.pool().clone())
}

#[tokio::test]
async fn release_jira_issue_deletes_claim_and_returns_204() {
    let (app, _temp_dir, sqlite_path) = automation_app();
    create_automation(&app, "nightly", "Nightly").await;
    let claims = jira_claim_store(&sqlite_path).await;
    let automation_id = AutomationId::new("nightly").expect("automation id should parse");
    let run_id = RunId::new();
    assert!(
        claims
            .claim(&automation_id, "ABC-1", run_id, chrono::Utc::now())
            .await
            .expect("claim should insert")
    );

    let response = app
        .oneshot(empty_request(
            Method::DELETE,
            "/automations/nightly/jira-issues/ABC-1",
        ))
        .await
        .expect("release should respond");
    response_status(
        response,
        StatusCode::NO_CONTENT,
        "DELETE /api/v1/automations/nightly/jira-issues/ABC-1",
    )
    .await;

    assert!(
        claims
            .by_run(run_id)
            .await
            .expect("by_run should query")
            .is_none()
    );
}

#[tokio::test]
async fn release_jira_issue_without_claim_is_404() {
    let (app, _temp_dir, _sqlite_path) = automation_app();
    create_automation(&app, "nightly", "Nightly").await;

    let response = app
        .oneshot(empty_request(
            Method::DELETE,
            "/automations/nightly/jira-issues/ABC-1",
        ))
        .await
        .expect("release should respond");
    let error = response_json(
        response,
        StatusCode::NOT_FOUND,
        "DELETE /api/v1/automations/nightly/jira-issues/ABC-1 without claim",
    )
    .await;
    assert_eq!(error["errors"][0]["code"], "jira_issue_claim_not_found");
}
