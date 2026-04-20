//! Conformance tests: spec ↔ router consistency.

#![allow(
    clippy::absolute_paths,
    clippy::default_trait_access,
    clippy::manual_assert,
    clippy::manual_let_else,
    reason = "These spec/router conformance tests prefer direct assertions over pedantic style lints."
)]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use fabro_server::install::{InstallAppState, build_install_router};
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::{build_router, create_app_state_with_env_lookup};
use serde_yaml::Value;
use tower::ServiceExt;

use super::helpers::{read_repo_file, test_app_state, test_settings};

fn load_spec() -> Value {
    let text = read_repo_file("docs/api-reference/fabro-api.yaml");
    serde_yaml::from_str(&text).expect("failed to parse spec")
}

fn resolve_path(path: &str) -> String {
    path.replace("{id}", "test-id")
        .replace("{qid}", "test-qid")
        .replace("{stageId}", "test-stage")
        .replace("{name}", "test-name")
        .replace("{slug}", "test-slug")
}

fn methods_for_path_item(item: &Value) -> Vec<Method> {
    const HTTP_METHODS: &[(&str, Method)] = &[
        ("get", Method::GET),
        ("post", Method::POST),
        ("put", Method::PUT),
        ("delete", Method::DELETE),
        ("patch", Method::PATCH),
    ];
    let Some(map) = item.as_mapping() else {
        return Vec::new();
    };
    HTTP_METHODS
        .iter()
        .filter(|(key, _)| map.contains_key(Value::String((*key).to_string())))
        .map(|(_, method)| method.clone())
        .collect()
}

fn path_item_has_tag(item: &Value, expected: &str) -> bool {
    let Some(map) = item.as_mapping() else {
        return false;
    };
    map.values().any(|operation| {
        operation
            .get("tags")
            .and_then(Value::as_sequence)
            .is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some(expected)))
    })
}

fn request_for(method: &Method, uri: &str) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = if method == Method::POST || method == Method::PUT || method == Method::PATCH {
        builder = builder.header("content-type", "application/json");
        Body::from("{}")
    } else {
        Body::empty()
    };
    builder
        .body(body)
        .expect("OpenAPI conformance request should build")
}

#[tokio::test]
async fn all_spec_routes_are_routable() {
    let spec = load_spec();
    let normal_app = build_router(test_app_state(), AuthMode::Disabled);
    let install_app = build_install_router(InstallAppState::for_test("test-install-token")).await;

    let paths = spec
        .get("paths")
        .and_then(Value::as_mapping)
        .expect("spec is missing `paths`");

    let mut checked = 0;
    for (path_key, item) in paths {
        let path = path_key.as_str().expect("path key must be a string");
        let uri = resolve_path(path);
        let app = if path_item_has_tag(item, "Install") {
            install_app.clone()
        } else {
            normal_app.clone()
        };
        for method in methods_for_path_item(item) {
            let response = app
                .clone()
                .oneshot(request_for(&method, &uri))
                .await
                .unwrap();

            assert_ne!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "Route {method} {path} returned 405 — not registered in the router"
            );
            checked += 1;
        }
    }

    assert!(checked > 0, "No routes were checked — is the spec empty?");
}

#[test]
fn github_webhook_spec_and_sdk_describe_a_json_body() {
    let spec = load_spec();
    let webhook_schema = spec["paths"]["/api/v1/webhooks/github"]["post"]["requestBody"]["content"]
        ["application/json"]["schema"]
        .clone();

    assert_eq!(
        webhook_schema.get("type").and_then(Value::as_str),
        Some("object"),
        "GitHub webhook request body should be modeled as JSON, not a binary file upload"
    );
    assert!(
        webhook_schema.get("format").is_none(),
        "GitHub webhook JSON schema should not declare a binary format"
    );

    let generated_client =
        read_repo_file("lib/packages/fabro-api-client/src/api/integrations-api.ts");
    assert!(
        !generated_client.contains("@param {File} body"),
        "generated TypeScript client should not expose the webhook body as File"
    );
    assert!(
        !generated_client.contains("receiveGithubWebhook: async (body: File"),
        "generated TypeScript client should not require File for a JSON webhook payload"
    );
}

#[tokio::test]
async fn github_webhook_spec_route_is_routable_when_webhook_secret_is_present() {
    let secret = "test-webhook-secret".to_string();
    let app = build_router(
        create_app_state_with_env_lookup(test_settings(), 5, move |name| {
            (name == "GITHUB_APP_WEBHOOK_SECRET").then(|| secret.clone())
        }),
        AuthMode::Disabled,
    );

    let response = app
        .oneshot(request_for(&Method::POST, "/api/v1/webhooks/github"))
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "Webhook spec route should be mounted when GITHUB_APP_WEBHOOK_SECRET is present"
    );
}

#[tokio::test]
async fn install_and_normal_routes_stay_isolated() {
    let spec = load_spec();
    let normal_app = build_router(test_app_state(), AuthMode::Disabled);
    let install_app = build_install_router(InstallAppState::for_test("test-install-token")).await;

    let paths = spec
        .get("paths")
        .and_then(Value::as_mapping)
        .expect("spec is missing `paths`");

    for (path_key, item) in paths {
        let path = path_key.as_str().expect("path key must be a string");
        let uri = resolve_path(path);
        let install_only = path_item_has_tag(item, "Install");
        let api_path = path.starts_with("/api/");

        for method in methods_for_path_item(item) {
            if install_only {
                let response = normal_app
                    .clone()
                    .oneshot(request_for(&method, &uri))
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "Install route {method} {path} should be absent from the normal router"
                );
            } else if api_path {
                let response = install_app
                    .clone()
                    .oneshot(request_for(&method, &uri))
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "Normal API route {method} {path} should be absent from the install router"
                );
            }
        }
    }
}

// Note: the earlier `server_settings_keys_match_openapi_spec` drift check
// was deleted in Stage 6.3b alongside the legacy flat `fabro_types::Settings`
// struct that it instantiated. The v2 `/api/v1/settings` and
// `/api/v1/runs/:id/settings` endpoints now return the freely-shaped
// `SettingsLayer` tree which the OpenAPI spec declares as
// `type: object, additionalProperties: true`, so there is nothing to diff
// at the property-key level.
