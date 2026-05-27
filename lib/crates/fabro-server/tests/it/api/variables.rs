use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, body_json, minimal_manifest_json, response_json, response_status,
    test_app_state,
};

fn json_request(method: Method, path: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(api(path))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(body).expect("request body should serialize"),
        ))
        .expect("request should build")
}

fn empty_request(method: Method, path: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(api(path))
        .body(Body::empty())
        .expect("request should build")
}

#[tokio::test]
async fn variables_crud_exposes_values() {
    let app = fabro_server::test_support::build_test_router(test_app_state());

    let list_empty = app
        .clone()
        .oneshot(empty_request(Method::GET, "/variables"))
        .await
        .expect("GET /variables should route");
    let body = response_json(list_empty, StatusCode::OK, "GET /api/v1/variables").await;
    assert_eq!(body, serde_json::json!({ "data": [] }));

    let create = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/variables",
            &serde_json::json!({
                "name": "DEPLOY_ENV",
                "value": "staging",
                "description": "Deployment target"
            }),
        ))
        .await
        .expect("POST /variables should route");
    let body = response_json(create, StatusCode::OK, "POST /api/v1/variables").await;
    assert_eq!(body["name"], "DEPLOY_ENV");
    assert_eq!(body["value"], "staging");
    assert_eq!(body["description"], "Deployment target");
    assert!(body.get("created_at").is_some());
    assert!(body.get("updated_at").is_some());

    let post_upsert = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/variables",
            &serde_json::json!({
                "name": "DEPLOY_ENV",
                "value": "qa"
            }),
        ))
        .await
        .expect("POST /variables upsert should route");
    let body = response_json(post_upsert, StatusCode::OK, "POST /api/v1/variables").await;
    assert_eq!(body["value"], "qa");
    assert_eq!(body["description"], "Deployment target");

    let get = app
        .clone()
        .oneshot(empty_request(Method::GET, "/variables/DEPLOY_ENV"))
        .await
        .expect("GET /variables/DEPLOY_ENV should route");
    let body = response_json(get, StatusCode::OK, "GET /api/v1/variables/DEPLOY_ENV").await;
    assert_eq!(body["value"], "qa");

    let update = app
        .clone()
        .oneshot(json_request(
            Method::PUT,
            "/variables/DEPLOY_ENV",
            &serde_json::json!({ "value": "production" }),
        ))
        .await
        .expect("PUT /variables/DEPLOY_ENV should route");
    let body = response_json(update, StatusCode::OK, "PUT /api/v1/variables/DEPLOY_ENV").await;
    assert_eq!(body["value"], "production");
    assert_eq!(body["description"], "Deployment target");

    let list = app
        .clone()
        .oneshot(empty_request(Method::GET, "/variables"))
        .await
        .expect("GET /variables should route");
    let body = response_json(list, StatusCode::OK, "GET /api/v1/variables").await;
    assert_eq!(body["data"][0]["name"], "DEPLOY_ENV");
    assert_eq!(body["data"][0]["value"], "production");

    let delete = app
        .clone()
        .oneshot(empty_request(Method::DELETE, "/variables/DEPLOY_ENV"))
        .await
        .expect("DELETE /variables/DEPLOY_ENV should route");
    response_status(
        delete,
        StatusCode::NO_CONTENT,
        "DELETE /api/v1/variables/DEPLOY_ENV",
    )
    .await;

    let missing = app
        .oneshot(empty_request(Method::GET, "/variables/DEPLOY_ENV"))
        .await
        .expect("GET /variables/DEPLOY_ENV should route");
    response_status(
        missing,
        StatusCode::NOT_FOUND,
        "GET /api/v1/variables/DEPLOY_ENV",
    )
    .await;
}

#[tokio::test]
async fn variables_validate_names_and_allow_empty_values() {
    let app = fabro_server::test_support::build_test_router(test_app_state());

    let invalid = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/variables",
            &serde_json::json!({
                "name": "1BAD",
                "value": "nope"
            }),
        ))
        .await
        .expect("POST /variables should route");
    response_status(invalid, StatusCode::BAD_REQUEST, "POST /api/v1/variables").await;

    let invalid_get = app
        .clone()
        .oneshot(empty_request(Method::GET, "/variables/1BAD"))
        .await
        .expect("GET /variables/1BAD should route");
    response_status(
        invalid_get,
        StatusCode::BAD_REQUEST,
        "GET /api/v1/variables/1BAD",
    )
    .await;

    let empty = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/variables",
            &serde_json::json!({
                "name": "EMPTY_ALLOWED",
                "value": ""
            }),
        ))
        .await
        .expect("POST /variables should route");
    let body = response_json(empty, StatusCode::OK, "POST /api/v1/variables").await;
    assert_eq!(body["value"], "");

    let missing_delete = app
        .oneshot(empty_request(Method::DELETE, "/variables/MISSING"))
        .await
        .expect("DELETE /variables/MISSING should route");
    response_status(
        missing_delete,
        StatusCode::NOT_FOUND,
        "DELETE /api/v1/variables/MISSING",
    )
    .await;
}

#[tokio::test]
async fn run_config_substitutes_variables_before_persisting_settings() {
    let app = fabro_server::test_support::build_test_router(test_app_state());

    let create_variable = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/variables",
            &serde_json::json!({
                "name": "RUNTIME_TOKEN",
                "value": "token-from-variable"
            }),
        ))
        .await
        .expect("POST /variables should route");
    response_status(create_variable, StatusCode::OK, "POST /api/v1/variables").await;

    let mut manifest = minimal_manifest_json(MINIMAL_DOT);
    manifest["configs"] = serde_json::json!([{
        "type": "project",
        "path": ".fabro/project.toml",
        "source": r#"
_version = 1

[run.environment]
id = "local"

[environments.local]
provider = "local"

[environments.local.env]
RUNTIME_TOKEN = "{{ vars.RUNTIME_TOKEN }}"
"#
    }]);

    let create_run = app
        .clone()
        .oneshot(json_request(Method::POST, "/runs", &manifest))
        .await
        .expect("POST /runs should route");
    let create_status = create_run.status();
    let create_body = body_json(create_run.into_body()).await;
    assert_eq!(create_status, StatusCode::CREATED, "{create_body}");
    let run_id = create_body["id"]
        .as_str()
        .expect("create run response should include id");

    let settings = app
        .oneshot(empty_request(
            Method::GET,
            &format!("/runs/{run_id}/settings"),
        ))
        .await
        .expect("GET run settings should route");
    let body = response_json(
        settings,
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/settings"),
    )
    .await;

    assert_eq!(
        body["run"]["environment"]["env"]["RUNTIME_TOKEN"],
        "token-from-variable"
    );
}
