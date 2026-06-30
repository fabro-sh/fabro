use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use fabro_server::server::build_router;
use fabro_server::test_support::{TestAppStateBuilder, build_test_router, test_auth_mode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, checked_response, minimal_manifest_json, response_json, response_status,
};

fn mcp_server_body(id: &str, display_name: &str) -> Value {
    json!({
        "id": id,
        "display_name": display_name,
        "description": "Production MCP server.",
        "transport": {
            "type": "http",
            "protocol": "streamable_http",
            "url": "https://example.com/mcp",
            "headers": {
                "X-Org": "fabro"
            }
        },
        "startup_timeout_secs": 10,
        "tool_timeout_secs": 60
    })
}

fn replacement_body(display_name: &str) -> Value {
    json!({
        "display_name": display_name,
        "description": null,
        "transport": {
            "type": "http",
            "protocol": "sse",
            "url": "https://example.com/mcp/v2",
            "headers": {}
        },
        "startup_timeout_secs": 15,
        "tool_timeout_secs": 90
    })
}

fn mcp_server_app() -> (axum::Router, tempfile::TempDir, PathBuf) {
    let temp_dir = tempfile::tempdir().expect("mcp server test tempdir should be created");
    let active_config_path = temp_dir.path().join("settings.toml");
    let mcp_dir = temp_dir.path().join("mcps");
    let state = TestAppStateBuilder::new()
        .active_config_path(active_config_path)
        .build();
    (build_test_router(state), temp_dir, mcp_dir)
}

fn json_request(method: Method, path: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(crate::helpers::api(path))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&body).expect("mcp server fixture should serialize"),
        ))
        .expect("mcp server JSON request should build")
}

fn empty_request(method: Method, path: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(crate::helpers::api(path))
        .body(Body::empty())
        .expect("mcp server request should build")
}

fn request_with_if_match(
    method: Method,
    path: &str,
    revision: &str,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(crate::helpers::api(path))
        .header(header::IF_MATCH, revision);
    let body = match body {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&value).expect("mcp server fixture should serialize"))
        }
        None => Body::empty(),
    };
    builder
        .body(body)
        .expect("mcp server If-Match request should build")
}

async fn create_mcp_server(app: &axum::Router, id: &str, name: &str) -> Value {
    let response = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/mcp-servers",
            &mcp_server_body(id, name),
        ))
        .await
        .expect("create mcp server should respond");
    response_json(response, StatusCode::CREATED, "POST /api/v1/mcp-servers").await
}

fn revision_from(body: &Value) -> &str {
    body["revision"]
        .as_str()
        .expect("mcp server response should include a revision")
}

async fn persisted_mcp_server_toml(mcp_dir: &Path, id: &str) -> toml::Value {
    let persisted = tokio::fs::read_to_string(mcp_dir.join(format!("{id}.toml")))
        .await
        .expect("persisted mcp server TOML should be readable");
    toml::from_str(&persisted).expect("persisted mcp server TOML should parse")
}

#[tokio::test]
async fn empty_mcp_server_list_returns_total_zero() {
    let (app, _temp_dir, _mcp_dir) = mcp_server_app();

    let response = app
        .oneshot(empty_request(Method::GET, "/mcp-servers"))
        .await
        .expect("list mcp servers should respond");
    let body = response_json(response, StatusCode::OK, "GET /api/v1/mcp-servers").await;

    assert_eq!(
        body,
        json!({
            "data": [],
            "meta": { "total": 0 }
        })
    );
}

#[tokio::test]
async fn create_mcp_server_returns_etag_and_persists_sibling_toml_file() {
    let (app, _temp_dir, mcp_dir) = mcp_server_app();

    let response = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/mcp-servers",
            &mcp_server_body("sentry", "Sentry"),
        ))
        .await
        .expect("create mcp server should respond");
    let response =
        checked_response(response, StatusCode::CREATED, "POST /api/v1/mcp-servers").await;
    let etag = response
        .headers()
        .get(header::ETAG)
        .expect("create mcp server should include ETag")
        .to_str()
        .expect("ETag should be ASCII")
        .to_string();
    let body = crate::helpers::body_json(response.into_body()).await;

    assert_eq!(body["id"], "sentry");
    assert_eq!(body["display_name"], "Sentry");
    assert_eq!(etag, format!("\"{}\"", revision_from(&body)));
    assert!(mcp_dir.join("sentry.toml").exists());

    // The response is the value-omitting view: header *names* are returned, but
    // the stored header value is not.
    assert_eq!(body["transport"]["header_keys"], json!(["X-Org"]));
    assert!(
        body["transport"].get("headers").is_none(),
        "response must not echo transport header values"
    );

    let persisted = persisted_mcp_server_toml(&mcp_dir, "sentry").await;
    assert_eq!(
        persisted.get("display_name").and_then(toml::Value::as_str),
        Some("Sentry")
    );
    assert!(persisted.get("id").is_none());
    assert!(persisted.get("revision").is_none());
    // The value the view omits is still persisted on disk for the runtime to use.
    assert_eq!(
        persisted
            .get("transport")
            .and_then(|transport| transport.get("headers"))
            .and_then(|headers| headers.get("X-Org"))
            .and_then(toml::Value::as_str),
        Some("fabro")
    );
}

#[tokio::test]
async fn mcp_server_round_trips_through_create_get_and_toml() {
    let (app, _temp_dir, mcp_dir) = mcp_server_app();

    let created = create_mcp_server(&app, "sentry", "Sentry").await;
    assert_eq!(created["transport"]["type"], "http");
    assert_eq!(created["transport"]["url"], "https://example.com/mcp");

    let response = app
        .clone()
        .oneshot(empty_request(Method::GET, "/mcp-servers/sentry"))
        .await
        .expect("get mcp server should respond");
    let retrieved = response_json(response, StatusCode::OK, "GET /api/v1/mcp-servers/sentry").await;
    assert_eq!(retrieved, created);

    let persisted = persisted_mcp_server_toml(&mcp_dir, "sentry").await;
    assert_eq!(
        persisted
            .get("transport")
            .and_then(|transport| transport.get("type"))
            .and_then(toml::Value::as_str),
        Some("http")
    );
}

#[tokio::test]
async fn list_mcp_servers_returns_items_sorted_by_id() {
    let (app, _temp_dir, _mcp_dir) = mcp_server_app();
    create_mcp_server(&app, "zulu", "Zulu").await;
    create_mcp_server(&app, "alpha", "Alpha").await;

    let response = app
        .oneshot(empty_request(Method::GET, "/mcp-servers"))
        .await
        .expect("list mcp servers should respond");
    let body = response_json(response, StatusCode::OK, "GET /api/v1/mcp-servers").await;

    assert_eq!(body["meta"]["total"], 2);
    assert_eq!(body["data"][0]["id"], "alpha");
    assert_eq!(body["data"][1]["id"], "zulu");
}

#[tokio::test]
async fn duplicate_mcp_server_create_returns_conflict() {
    let (app, _temp_dir, _mcp_dir) = mcp_server_app();
    create_mcp_server(&app, "sentry", "Sentry").await;

    let response = app
        .oneshot(json_request(
            Method::POST,
            "/mcp-servers",
            &mcp_server_body("sentry", "Duplicate"),
        ))
        .await
        .expect("duplicate create should respond");

    response_status(
        response,
        StatusCode::CONFLICT,
        "POST /api/v1/mcp-servers duplicate",
    )
    .await;
}

#[tokio::test]
async fn get_mcp_server_returns_current_etag() {
    let (app, _temp_dir, _mcp_dir) = mcp_server_app();
    let created = create_mcp_server(&app, "sentry", "Sentry").await;
    let revision = revision_from(&created);

    let response = app
        .oneshot(empty_request(Method::GET, "/mcp-servers/sentry"))
        .await
        .expect("get mcp server should respond");
    let response =
        checked_response(response, StatusCode::OK, "GET /api/v1/mcp-servers/sentry").await;

    assert_eq!(
        response
            .headers()
            .get(header::ETAG)
            .expect("GET mcp server should include ETag"),
        &format!("\"{revision}\"")
    );
    let body = crate::helpers::body_json(response.into_body()).await;
    assert_eq!(body["revision"], revision);
}

#[tokio::test]
async fn get_missing_mcp_server_returns_not_found() {
    let (app, _temp_dir, _mcp_dir) = mcp_server_app();

    let response = app
        .oneshot(empty_request(Method::GET, "/mcp-servers/missing"))
        .await
        .expect("get missing mcp server should respond");

    response_status(
        response,
        StatusCode::NOT_FOUND,
        "GET /api/v1/mcp-servers/missing",
    )
    .await;
}

#[tokio::test]
async fn replace_mcp_server_accepts_unquoted_if_match_and_returns_new_etag() {
    let (app, _temp_dir, _mcp_dir) = mcp_server_app();
    let created = create_mcp_server(&app, "sentry", "Sentry").await;
    let revision = revision_from(&created);

    let response = app
        .oneshot(request_with_if_match(
            Method::PUT,
            "/mcp-servers/sentry",
            revision,
            Some(replacement_body("Sentry v2")),
        ))
        .await
        .expect("replace mcp server should respond");
    let response =
        checked_response(response, StatusCode::OK, "PUT /api/v1/mcp-servers/sentry").await;
    let etag = response
        .headers()
        .get(header::ETAG)
        .expect("PUT mcp server should include ETag")
        .to_str()
        .expect("ETag should be ASCII")
        .to_string();
    let body = crate::helpers::body_json(response.into_body()).await;

    assert_eq!(body["display_name"], "Sentry v2");
    assert_eq!(body["transport"]["protocol"], "sse");
    assert_ne!(body["revision"], revision);
    assert_eq!(etag, format!("\"{}\"", revision_from(&body)));
}

#[tokio::test]
async fn replace_mcp_server_accepts_quoted_if_match() {
    let (app, _temp_dir, _mcp_dir) = mcp_server_app();
    let created = create_mcp_server(&app, "sentry", "Sentry").await;
    let revision = revision_from(&created);

    let response = app
        .oneshot(request_with_if_match(
            Method::PUT,
            "/mcp-servers/sentry",
            &format!("\"{revision}\""),
            Some(replacement_body("Sentry v2")),
        ))
        .await
        .expect("replace mcp server with quoted If-Match should respond");

    response_status(
        response,
        StatusCode::OK,
        "PUT /api/v1/mcp-servers/sentry quoted If-Match",
    )
    .await;
}

#[tokio::test]
async fn stale_mcp_server_replace_returns_conflict() {
    let (app, _temp_dir, _mcp_dir) = mcp_server_app();
    let created = create_mcp_server(&app, "sentry", "Sentry").await;
    let stale_revision = revision_from(&created).to_string();

    let replaced = app
        .clone()
        .oneshot(request_with_if_match(
            Method::PUT,
            "/mcp-servers/sentry",
            &stale_revision,
            Some(replacement_body("Updated")),
        ))
        .await
        .expect("first replace should respond");
    response_status(
        replaced,
        StatusCode::OK,
        "PUT /api/v1/mcp-servers/sentry first replace",
    )
    .await;

    let response = app
        .oneshot(request_with_if_match(
            Method::PUT,
            "/mcp-servers/sentry",
            &stale_revision,
            Some(replacement_body("Stale")),
        ))
        .await
        .expect("stale replace should respond");

    response_status(
        response,
        StatusCode::CONFLICT,
        "PUT /api/v1/mcp-servers/sentry stale",
    )
    .await;
}

#[tokio::test]
async fn replace_and_delete_mcp_server_require_if_match() {
    let (app, _temp_dir, _mcp_dir) = mcp_server_app();
    create_mcp_server(&app, "sentry", "Sentry").await;

    let replace_response = app
        .clone()
        .oneshot(json_request(
            Method::PUT,
            "/mcp-servers/sentry",
            &replacement_body("Updated"),
        ))
        .await
        .expect("replace without If-Match should respond");
    response_status(
        replace_response,
        StatusCode::PRECONDITION_REQUIRED,
        "PUT /api/v1/mcp-servers/sentry without If-Match",
    )
    .await;

    let delete_response = app
        .oneshot(empty_request(Method::DELETE, "/mcp-servers/sentry"))
        .await
        .expect("delete without If-Match should respond");
    response_status(
        delete_response,
        StatusCode::PRECONDITION_REQUIRED,
        "DELETE /api/v1/mcp-servers/sentry without If-Match",
    )
    .await;
}

#[tokio::test]
async fn delete_mcp_server_removes_file_and_resource() {
    let (app, _temp_dir, mcp_dir) = mcp_server_app();
    let created = create_mcp_server(&app, "sentry", "Sentry").await;
    let revision = revision_from(&created);

    let response = app
        .clone()
        .oneshot(request_with_if_match(
            Method::DELETE,
            "/mcp-servers/sentry",
            &format!("\"{revision}\""),
            None,
        ))
        .await
        .expect("delete mcp server should respond");
    response_status(
        response,
        StatusCode::NO_CONTENT,
        "DELETE /api/v1/mcp-servers/sentry",
    )
    .await;

    assert!(!mcp_dir.join("sentry.toml").exists());
    let response = app
        .oneshot(empty_request(Method::GET, "/mcp-servers/sentry"))
        .await
        .expect("get deleted mcp server should respond");
    response_status(
        response,
        StatusCode::NOT_FOUND,
        "GET /api/v1/mcp-servers/sentry after delete",
    )
    .await;
}

#[tokio::test]
async fn empty_mcp_server_name_is_unprocessable() {
    let (app, _temp_dir, _mcp_dir) = mcp_server_app();
    let mut body = mcp_server_body("sentry", "Sentry");
    body["display_name"] = json!(" ");

    let response = app
        .oneshot(json_request(Method::POST, "/mcp-servers", &body))
        .await
        .expect("empty mcp server name create should respond");

    response_status(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /api/v1/mcp-servers empty name",
    )
    .await;
}

#[tokio::test]
async fn empty_transport_command_is_unprocessable() {
    let (app, _temp_dir, _mcp_dir) = mcp_server_app();
    let mut body = mcp_server_body("local", "Local");
    body["transport"] = json!({
        "type": "stdio",
        "command": [],
        "env": {}
    });

    let response = app
        .oneshot(json_request(Method::POST, "/mcp-servers", &body))
        .await
        .expect("empty transport command create should respond");

    response_status(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /api/v1/mcp-servers empty transport command",
    )
    .await;
}

#[tokio::test]
async fn unknown_transport_type_is_unprocessable() {
    let (app, _temp_dir, _mcp_dir) = mcp_server_app();
    let mut body = mcp_server_body("sentry", "Sentry");
    body["transport"] = json!({
        "type": "carrier-pigeon",
        "url": "https://example.com/mcp"
    });

    let response = app
        .oneshot(json_request(Method::POST, "/mcp-servers", &body))
        .await
        .expect("unknown transport type create should respond");

    response_status(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /api/v1/mcp-servers unknown transport type",
    )
    .await;
}

#[tokio::test]
async fn unknown_transport_field_is_unprocessable() {
    let (app, _temp_dir, _mcp_dir) = mcp_server_app();
    let mut body = mcp_server_body("sentry", "Sentry");
    body["transport"]["unexpected"] = json!("typo");

    let response = app
        .oneshot(json_request(Method::POST, "/mcp-servers", &body))
        .await
        .expect("unknown transport field create should respond");

    response_status(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /api/v1/mcp-servers unknown transport field",
    )
    .await;
}

#[tokio::test]
async fn invalid_mcp_server_id_is_bad_request() {
    let (app, _temp_dir, _mcp_dir) = mcp_server_app();

    let response = app
        .oneshot(empty_request(Method::GET, "/mcp-servers/Bad_Id"))
        .await
        .expect("invalid id get should respond");

    response_status(
        response,
        StatusCode::BAD_REQUEST,
        "GET /api/v1/mcp-servers/Bad_Id",
    )
    .await;
}

#[tokio::test]
async fn created_mcp_server_can_be_referenced_by_manifest_validation() {
    let (app, _temp_dir, _mcp_dir) = mcp_server_app();
    create_mcp_server(&app, "sentry", "Sentry").await;

    let mut manifest = minimal_manifest_json(MINIMAL_DOT);
    manifest["workflows"]["workflow.fabro"]["config"] = json!({
        "path": "workflow.toml",
        "source": r#"
_version = 1

[run.agent.mcps.sentry]
id = "sentry"
"#
    });

    let response = app
        .oneshot(json_request(Method::POST, "/validate", &manifest))
        .await
        .expect("manifest validation should respond");
    let body = response_json(response, StatusCode::OK, "POST /api/v1/validate").await;

    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn mcp_server_store_malformed_persisted_toml_fails_startup() {
    let temp_dir = tempfile::tempdir().expect("mcp server test tempdir should be created");
    let mcp_dir = temp_dir.path().join("mcps");
    tokio::fs::create_dir_all(&mcp_dir)
        .await
        .expect("mcp server dir should be created");
    tokio::fs::write(mcp_dir.join("broken.toml"), "not valid toml =")
        .await
        .expect("broken mcp server fixture should be written");

    let result = TestAppStateBuilder::new()
        .active_config_path(temp_dir.path().join("settings.toml"))
        .try_build();

    assert!(result.is_err());
}

#[tokio::test]
async fn mcp_servers_routes_require_authenticated_user() {
    let temp_dir = tempfile::tempdir().expect("mcp server test tempdir should be created");
    let state = TestAppStateBuilder::new()
        .active_config_path(temp_dir.path().join("settings.toml"))
        .build();
    let app = build_router(state, test_auth_mode());

    let response = app
        .oneshot(empty_request(Method::GET, "/mcp-servers"))
        .await
        .expect("unauthenticated mcp server list should respond");

    response_status(
        response,
        StatusCode::UNAUTHORIZED,
        "GET /api/v1/mcp-servers without auth",
    )
    .await;
}
