//! Tests that paginated list endpoints return `{ data, meta: { has_more } }`.

#![allow(clippy::absolute_paths)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::build_router;
use tower::ServiceExt;

use super::helpers::test_app_state;

async fn get_json(app: axum::Router, uri: &str) -> serde_json::Value {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-fabro-demo", "1")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "GET {uri} failed");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

/// Assert that a value has the paginated shape: `{ data: [...], meta: {
/// has_more: bool } }`
fn assert_paginated_shape(json: &serde_json::Value, context: &str) {
    assert!(json.get("data").is_some(), "{context}: missing 'data' key");
    assert!(json["data"].is_array(), "{context}: 'data' is not an array");
    assert!(json.get("meta").is_some(), "{context}: missing 'meta' key");
    assert!(
        json["meta"].get("has_more").is_some(),
        "{context}: missing 'meta.has_more'"
    );
    assert!(
        json["meta"]["has_more"].is_boolean(),
        "{context}: 'meta.has_more' is not boolean"
    );
}

struct PaginatedEndpoint {
    path: &'static str,
    name: &'static str,
}

const ENDPOINTS: &[PaginatedEndpoint] = &[
    PaginatedEndpoint {
        path: "/api/v1/insights/queries",
        name: "listSavedQueries",
    },
    PaginatedEndpoint {
        path: "/api/v1/insights/history",
        name: "listQueryHistory",
    },
    PaginatedEndpoint {
        path: "/api/v1/models",
        name: "listModels",
    },
    PaginatedEndpoint {
        path: "/api/v1/runs/run-1/stages/detect-drift/turns",
        name: "listStageTurns",
    },
    PaginatedEndpoint {
        path: "/api/v1/runs/run-1/questions",
        name: "listRunQuestions",
    },
    PaginatedEndpoint {
        path: "/api/v1/runs/run-1/stages",
        name: "listRunStages",
    },
];

#[tokio::test]
async fn paginated_endpoints_return_correct_shape() {
    let state = test_app_state();
    let app = build_router(state, AuthMode::Disabled);

    for ep in ENDPOINTS {
        // Default request: paginated shape, has_more = false (fixtures fit in default
        // page)
        let json = get_json(app.clone(), ep.path).await;
        assert_paginated_shape(&json, ep.name);
        assert_eq!(
            json["meta"]["has_more"], false,
            "{}: default request should have has_more=false",
            ep.name
        );

        // limit=1: at most 1 item, has_more = true (all fixtures have >1 item)
        let uri = if ep.path.contains('?') {
            format!("{}&page[limit]=1", ep.path)
        } else {
            format!("{}?page[limit]=1", ep.path)
        };
        let json = get_json(app.clone(), &uri).await;
        assert_paginated_shape(&json, &format!("{} limit=1", ep.name));
        assert!(
            json["data"].as_array().unwrap().len() <= 1,
            "{}: limit=1 returned more than 1 item",
            ep.name
        );
        assert_eq!(
            json["meta"]["has_more"], true,
            "{}: limit=1 should have has_more=true",
            ep.name
        );
    }
}
