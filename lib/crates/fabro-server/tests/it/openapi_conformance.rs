//! Conformance tests: spec ↔ router consistency.

#![allow(
    clippy::absolute_paths,
    clippy::default_trait_access,
    clippy::manual_assert,
    clippy::manual_let_else
)]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use fabro_server::install::{InstallAppState, build_install_router};
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::build_router;
use serde_yaml::Value;
use tower::ServiceExt;

use super::helpers::test_app_state;

fn load_spec() -> Value {
    let spec_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("docs/api-reference/fabro-api.yaml");
    let text = std::fs::read_to_string(&spec_path).expect("failed to read spec");
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
    builder.body(body).unwrap()
}

#[tokio::test]
async fn all_spec_routes_are_routable() {
    let spec = load_spec();
    let normal_app = build_router(test_app_state(), AuthMode::Disabled);
    let install_app = build_install_router(InstallAppState::for_test("test-install-token"));

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

#[tokio::test]
async fn install_and_normal_routes_stay_isolated() {
    let spec = load_spec();
    let normal_app = build_router(test_app_state(), AuthMode::Disabled);
    let install_app = build_install_router(InstallAppState::for_test("test-install-token"));

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
