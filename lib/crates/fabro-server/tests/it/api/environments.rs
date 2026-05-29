use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use fabro_config::{RunEnvironmentLayer, RunLayer};
use fabro_server::server::build_router;
use fabro_server::test_support::{
    TestAppStateBuilder, build_test_router, default_test_server_settings, test_auth_mode,
};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::helpers::{api, checked_response, response_json, response_status};

fn environment_settings(provider: &str) -> Value {
    json!({
        "provider": provider,
        "image": {
            "docker": if provider == "docker" { json!("alpine:3.20") } else { Value::Null },
            "dockerfile": null
        },
        "resources": {
            "cpu": null,
            "memory": null,
            "disk": null
        },
        "network": {
            "mode": "allow_all",
            "allow": []
        },
        "lifecycle": {
            "preserve": false,
            "stop_on_terminal": true,
            "auto_stop": null
        },
        "labels": {},
        "volumes": [],
        "env": {}
    })
}

fn environment_body(id: &str, provider: &str) -> Value {
    let mut body = environment_settings(provider);
    body["id"] = json!(id);
    body
}

fn environment_app() -> (axum::Router, tempfile::TempDir, PathBuf) {
    let temp_dir = tempfile::tempdir().expect("environment test tempdir should be created");
    let active_config_path = temp_dir.path().join("settings.toml");
    let environment_dir = temp_dir.path().join("environments");
    let state = TestAppStateBuilder::new()
        .active_config_path(active_config_path)
        .build();
    (build_test_router(state), temp_dir, environment_dir)
}

fn environment_app_with_default_environment(
    environment_id: &str,
) -> (axum::Router, tempfile::TempDir) {
    let temp_dir = tempfile::tempdir().expect("environment test tempdir should be created");
    let active_config_path = temp_dir.path().join("settings.toml");
    let manifest_run_defaults = RunLayer {
        environment: Some(RunEnvironmentLayer {
            id: Some(environment_id.to_string()),
            ..RunEnvironmentLayer::default()
        }),
        ..RunLayer::default()
    };
    let state = TestAppStateBuilder::new()
        .runtime_settings(default_test_server_settings(), manifest_run_defaults)
        .active_config_path(active_config_path)
        .build();
    (build_test_router(state), temp_dir)
}

fn json_request(method: Method, path: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(api(path))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(body).expect("environment fixture should serialize"),
        ))
        .expect("environment JSON request should build")
}

fn empty_request(method: Method, path: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(api(path))
        .body(Body::empty())
        .expect("environment request should build")
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
            Body::from(serde_json::to_vec(&value).expect("environment fixture should serialize"))
        }
        None => Body::empty(),
    };
    builder
        .body(body)
        .expect("environment If-Match request should build")
}

async fn create_environment(app: &axum::Router, id: &str, provider: &str) -> Value {
    create_environment_with_body(app, &environment_body(id, provider)).await
}

async fn create_environment_with_body(app: &axum::Router, body: &Value) -> Value {
    let response = app
        .clone()
        .oneshot(json_request(Method::POST, "/environments", body))
        .await
        .expect("create environment should respond");
    response_json(response, StatusCode::CREATED, "POST /api/v1/environments").await
}

fn revision_from(body: &Value) -> &str {
    body["revision"]
        .as_str()
        .expect("environment response should include a revision")
}

async fn persisted_environment_toml(environment_dir: &Path, id: &str) -> toml::Value {
    let persisted = tokio::fs::read_to_string(environment_dir.join(format!("{id}.toml")))
        .await
        .expect("persisted environment TOML should be readable");
    toml::from_str(&persisted).expect("persisted environment TOML should parse")
}

async fn system_info(app: &axum::Router) -> Value {
    let response = app
        .clone()
        .oneshot(empty_request(Method::GET, "/system/info"))
        .await
        .expect("system info should respond");
    response_json(response, StatusCode::OK, "GET /api/v1/system/info").await
}

#[tokio::test]
async fn list_environments_returns_seeded_catalog_sorted_by_id() {
    let (app, _temp_dir, _environment_dir) = environment_app();

    let response = app
        .oneshot(empty_request(Method::GET, "/environments"))
        .await
        .expect("list environments should respond");
    let body = response_json(response, StatusCode::OK, "GET /api/v1/environments").await;

    assert_eq!(body["meta"]["total"], 4);
    assert_eq!(
        body["data"]
            .as_array()
            .expect("environment list data should be an array")
            .iter()
            .map(|environment| environment["id"]
                .as_str()
                .expect("environment should have id"))
            .collect::<Vec<_>>(),
        vec!["daytona", "default", "docker", "local"]
    );
}

#[tokio::test]
async fn create_environment_persists_sibling_toml_and_is_visible() {
    let (app, _temp_dir, environment_dir) = environment_app();

    let created = create_environment(&app, "custom-env", "docker").await;

    assert_eq!(created["id"], "custom-env");
    assert_eq!(created["provider"], "docker");
    assert!(environment_dir.join("custom-env.toml").exists());

    let retrieved = app
        .clone()
        .oneshot(empty_request(Method::GET, "/environments/custom-env"))
        .await
        .expect("get environment should respond");
    let retrieved = response_json(
        retrieved,
        StatusCode::OK,
        "GET /api/v1/environments/custom-env",
    )
    .await;
    assert_eq!(retrieved["id"], "custom-env");

    let list = app
        .oneshot(empty_request(Method::GET, "/environments"))
        .await
        .expect("list environments should respond");
    let list = response_json(list, StatusCode::OK, "GET /api/v1/environments").await;
    assert_eq!(list["meta"]["total"], 5);
    assert!(
        list["data"]
            .as_array()
            .expect("environment list data should be an array")
            .iter()
            .any(|environment| environment["id"] == "custom-env")
    );

    let persisted = persisted_environment_toml(&environment_dir, "custom-env").await;
    assert_eq!(
        persisted.get("provider").and_then(toml::Value::as_str),
        Some("docker")
    );
    assert!(persisted.get("id").is_none());
    assert!(persisted.get("revision").is_none());
}

#[tokio::test]
async fn get_environment_returns_current_etag() {
    let (app, _temp_dir, _environment_dir) = environment_app();
    let created = create_environment(&app, "etag-env", "local").await;
    let revision = revision_from(&created);

    let response = app
        .oneshot(empty_request(Method::GET, "/environments/etag-env"))
        .await
        .expect("get environment should respond");
    let response = checked_response(
        response,
        StatusCode::OK,
        "GET /api/v1/environments/etag-env",
    )
    .await;

    assert_eq!(
        response
            .headers()
            .get(header::ETAG)
            .expect("GET environment should include ETag"),
        &format!("\"{revision}\"")
    );
    let body = crate::helpers::body_json(response.into_body()).await;
    assert_eq!(body["revision"], revision);
}

#[tokio::test]
async fn replace_environment_updates_file_and_returns_new_etag() {
    let (app, _temp_dir, environment_dir) = environment_app();
    let created = create_environment(&app, "replace-env", "docker").await;
    let revision = revision_from(&created);
    let mut replacement = environment_settings("local");
    replacement["labels"] = json!({ "tier": "dev" });

    let response = app
        .oneshot(request_with_if_match(
            Method::PUT,
            "/environments/replace-env",
            revision,
            Some(replacement),
        ))
        .await
        .expect("replace environment should respond");
    let response = checked_response(
        response,
        StatusCode::OK,
        "PUT /api/v1/environments/replace-env",
    )
    .await;
    let etag = response
        .headers()
        .get(header::ETAG)
        .expect("PUT environment should include ETag")
        .to_str()
        .expect("ETag should be ASCII")
        .to_string();
    let body = crate::helpers::body_json(response.into_body()).await;

    assert_eq!(body["provider"], "local");
    assert_eq!(body["labels"]["tier"], "dev");
    assert_ne!(body["revision"], revision);
    assert_eq!(etag, format!("\"{}\"", revision_from(&body)));
    let persisted = persisted_environment_toml(&environment_dir, "replace-env").await;
    assert_eq!(
        persisted
            .get("labels")
            .and_then(toml::Value::as_table)
            .and_then(|labels| labels.get("tier"))
            .and_then(toml::Value::as_str),
        Some("dev")
    );
}

#[tokio::test]
async fn replace_and_delete_environment_require_if_match() {
    let (app, _temp_dir, _environment_dir) = environment_app();
    create_environment(&app, "match-env", "local").await;

    let replace_response = app
        .clone()
        .oneshot(json_request(
            Method::PUT,
            "/environments/match-env",
            &environment_settings("docker"),
        ))
        .await
        .expect("replace without If-Match should respond");
    response_status(
        replace_response,
        StatusCode::PRECONDITION_REQUIRED,
        "PUT /api/v1/environments/match-env without If-Match",
    )
    .await;

    let delete_response = app
        .oneshot(empty_request(Method::DELETE, "/environments/match-env"))
        .await
        .expect("delete without If-Match should respond");
    response_status(
        delete_response,
        StatusCode::PRECONDITION_REQUIRED,
        "DELETE /api/v1/environments/match-env without If-Match",
    )
    .await;
}

#[tokio::test]
async fn stale_environment_replace_and_delete_return_conflict() {
    let (app, _temp_dir, _environment_dir) = environment_app();
    let created = create_environment(&app, "stale-env", "docker").await;
    let stale_revision = revision_from(&created).to_string();

    let replaced = app
        .clone()
        .oneshot(request_with_if_match(
            Method::PUT,
            "/environments/stale-env",
            &stale_revision,
            Some(environment_settings("local")),
        ))
        .await
        .expect("first replace should respond");
    response_status(
        replaced,
        StatusCode::OK,
        "PUT /api/v1/environments/stale-env first replace",
    )
    .await;

    let stale_replace = app
        .clone()
        .oneshot(request_with_if_match(
            Method::PUT,
            "/environments/stale-env",
            &stale_revision,
            Some(environment_settings("docker")),
        ))
        .await
        .expect("stale replace should respond");
    response_status(
        stale_replace,
        StatusCode::CONFLICT,
        "PUT /api/v1/environments/stale-env stale",
    )
    .await;

    let stale_delete = app
        .oneshot(request_with_if_match(
            Method::DELETE,
            "/environments/stale-env",
            &stale_revision,
            None,
        ))
        .await
        .expect("stale delete should respond");
    response_status(
        stale_delete,
        StatusCode::CONFLICT,
        "DELETE /api/v1/environments/stale-env stale",
    )
    .await;
}

#[tokio::test]
async fn duplicate_environment_create_returns_conflict() {
    let (app, _temp_dir, _environment_dir) = environment_app();
    create_environment(&app, "duplicate-env", "local").await;

    let response = app
        .oneshot(json_request(
            Method::POST,
            "/environments",
            &environment_body("duplicate-env", "docker"),
        ))
        .await
        .expect("duplicate create should respond");

    response_status(
        response,
        StatusCode::CONFLICT,
        "POST /api/v1/environments duplicate",
    )
    .await;
}

#[tokio::test]
async fn invalid_environment_id_and_if_match_return_bad_request() {
    let (app, _temp_dir, _environment_dir) = environment_app();

    let invalid_id = app
        .clone()
        .oneshot(empty_request(Method::GET, "/environments/Bad!"))
        .await
        .expect("invalid id request should respond");
    response_status(
        invalid_id,
        StatusCode::BAD_REQUEST,
        "GET /api/v1/environments/Bad!",
    )
    .await;

    create_environment(&app, "header-env", "local").await;
    let invalid_header = app
        .oneshot(request_with_if_match(
            Method::PUT,
            "/environments/header-env",
            "not-a-revision",
            Some(environment_settings("docker")),
        ))
        .await
        .expect("invalid If-Match request should respond");
    response_status(
        invalid_header,
        StatusCode::BAD_REQUEST,
        "PUT /api/v1/environments/header-env invalid If-Match",
    )
    .await;
}

#[tokio::test]
async fn invalid_environment_settings_return_unprocessable_entity() {
    let (app, _temp_dir, _environment_dir) = environment_app();
    let mut body = environment_body("invalid-env", "local");
    body["network"]["mode"] = json!("block");

    let response = app
        .oneshot(json_request(Method::POST, "/environments", &body))
        .await
        .expect("invalid environment create should respond");

    response_status(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /api/v1/environments invalid settings",
    )
    .await;
}

#[tokio::test]
async fn dockerfile_path_over_rest_is_rejected_without_persisting_or_exposing_contents() {
    let (app, temp_dir, environment_dir) = environment_app();
    tokio::fs::write(
        temp_dir.path().join("Dockerfile"),
        "FROM private.example/secret\n",
    )
    .await
    .expect("secret Dockerfile fixture should be written");
    let mut body = environment_body("path-env", "docker");
    body["image"]["docker"] = Value::Null;
    body["image"]["dockerfile"] = json!({
        "type": "path",
        "path": "Dockerfile"
    });

    let response = app
        .clone()
        .oneshot(json_request(Method::POST, "/environments", &body))
        .await
        .expect("path Dockerfile create should respond");
    let error = response_json(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /api/v1/environments Dockerfile path",
    )
    .await;

    assert!(!environment_dir.join("path-env.toml").exists());
    assert!(
        !serde_json::to_string(&error)
            .expect("error body should serialize")
            .contains("private.example/secret")
    );
    let list = app
        .oneshot(empty_request(Method::GET, "/environments"))
        .await
        .expect("list environments should respond");
    let list = response_json(list, StatusCode::OK, "GET /api/v1/environments").await;
    assert!(
        !list["data"]
            .as_array()
            .expect("environment list data should be an array")
            .iter()
            .any(|environment| environment["id"] == "path-env")
    );
}

#[tokio::test]
async fn delete_environment_removes_non_default_and_default_is_protected() {
    let (app, _temp_dir, environment_dir) = environment_app();
    let created = create_environment(&app, "delete-env", "local").await;
    let revision = revision_from(&created);

    let response = app
        .clone()
        .oneshot(request_with_if_match(
            Method::DELETE,
            "/environments/delete-env",
            &format!("\"{revision}\""),
            None,
        ))
        .await
        .expect("delete environment should respond");
    response_status(
        response,
        StatusCode::NO_CONTENT,
        "DELETE /api/v1/environments/delete-env",
    )
    .await;

    assert!(!environment_dir.join("delete-env.toml").exists());
    let missing = app
        .clone()
        .oneshot(empty_request(Method::GET, "/environments/delete-env"))
        .await
        .expect("get deleted environment should respond");
    response_status(
        missing,
        StatusCode::NOT_FOUND,
        "GET /api/v1/environments/delete-env after delete",
    )
    .await;

    let default = app
        .clone()
        .oneshot(empty_request(Method::GET, "/environments/default"))
        .await
        .expect("get default environment should respond");
    let default = response_json(default, StatusCode::OK, "GET /api/v1/environments/default").await;
    let protected = app
        .oneshot(request_with_if_match(
            Method::DELETE,
            "/environments/default",
            revision_from(&default),
            None,
        ))
        .await
        .expect("delete default environment should respond");
    response_status(
        protected,
        StatusCode::CONFLICT,
        "DELETE /api/v1/environments/default",
    )
    .await;
}

#[tokio::test]
async fn environment_routes_require_authenticated_user() {
    let temp_dir = tempfile::tempdir().expect("environment test tempdir should be created");
    let state = TestAppStateBuilder::new()
        .active_config_path(temp_dir.path().join("settings.toml"))
        .build();
    let app = build_router(state, test_auth_mode());

    let response = app
        .oneshot(empty_request(Method::GET, "/environments"))
        .await
        .expect("unauthenticated environment list should respond");

    response_status(
        response,
        StatusCode::UNAUTHORIZED,
        "GET /api/v1/environments without auth",
    )
    .await;
}

#[tokio::test]
async fn create_environment_refreshes_cached_manifest_run_settings() {
    let (app, _temp_dir) = environment_app_with_default_environment("api-default");

    let before = system_info(&app).await;
    assert_eq!(before["sandbox_provider"], "local");

    create_environment(&app, "api-default", "daytona").await;

    let after = system_info(&app).await;
    assert_eq!(after["sandbox_provider"], "daytona");
}
