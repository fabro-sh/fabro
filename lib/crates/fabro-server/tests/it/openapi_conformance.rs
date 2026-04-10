//! Conformance tests: spec ↔ router consistency.

#![allow(
    clippy::absolute_paths,
    clippy::default_trait_access,
    clippy::manual_assert,
    clippy::manual_let_else
)]

use super::helpers::test_app_state;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::build_router;
use tower::ServiceExt;

fn load_spec() -> openapiv3::OpenAPI {
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

fn methods_for_path_item(item: &openapiv3::PathItem) -> Vec<Method> {
    let mut methods = Vec::new();
    if item.get.is_some() {
        methods.push(Method::GET);
    }
    if item.post.is_some() {
        methods.push(Method::POST);
    }
    if item.put.is_some() {
        methods.push(Method::PUT);
    }
    if item.delete.is_some() {
        methods.push(Method::DELETE);
    }
    if item.patch.is_some() {
        methods.push(Method::PATCH);
    }
    methods
}

#[tokio::test]
async fn all_spec_routes_are_routable() {
    let spec = load_spec();
    let state = test_app_state();
    let app = build_router(state, AuthMode::Disabled);

    let mut checked = 0;
    for (path, item) in &spec.paths.paths {
        let path_item = match item {
            openapiv3::ReferenceOr::Item(item) => item,
            openapiv3::ReferenceOr::Reference { .. } => continue,
        };

        let uri = resolve_path(path);
        for method in methods_for_path_item(path_item) {
            let mut builder = Request::builder().method(&method).uri(&uri);

            let body = if method == Method::POST {
                builder = builder.header("content-type", "application/json");
                Body::from("{}")
            } else {
                Body::empty()
            };

            let req = builder.body(body).unwrap();
            let response = app.clone().oneshot(req).await.unwrap();

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

// Note: the earlier `server_settings_keys_match_openapi_spec` drift check
// was deleted in Stage 6.3b alongside the legacy flat `fabro_types::Settings`
// struct that it instantiated. The v2 `/api/v1/settings` and
// `/api/v1/runs/:id/settings` endpoints now return the freely-shaped
// `SettingsLayer` tree which the OpenAPI spec declares as
// `type: object, additionalProperties: true`, so there is nothing to diff
// at the property-key level.
