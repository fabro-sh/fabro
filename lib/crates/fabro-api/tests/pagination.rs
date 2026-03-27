//! Tests that paginated list endpoints return `{ data, meta: { has_more } }`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_api::jwt_auth::AuthMode;
use fabro_api::server::{build_router, create_app_state};
use fabro_workflows::pipeline::LlmSpec;
use tower::ServiceExt;

fn test_llm_spec() -> LlmSpec {
    LlmSpec {
        model: "test-model".to_string(),
        provider: fabro_llm::Provider::Anthropic,
        fallback_chain: Vec::new(),
        mcp_servers: Vec::new(),
        dry_run: true,
    }
}

async fn test_db() -> sqlx::SqlitePool {
    let pool = fabro_db::connect_memory().await.unwrap();
    fabro_db::initialize_db(&pool).await.unwrap();
    pool
}

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

/// Assert that a value has the paginated shape: `{ data: [...], meta: { has_more: bool } }`
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
        path: "/workflows",
        name: "listWorkflows",
    },
    PaginatedEndpoint {
        path: "/workflows/implement/runs",
        name: "listWorkflowRuns",
    },
    PaginatedEndpoint {
        path: "/retros",
        name: "listRetros",
    },
    PaginatedEndpoint {
        path: "/sessions",
        name: "listSessions",
    },
    PaginatedEndpoint {
        path: "/insights/queries",
        name: "listSavedQueries",
    },
    PaginatedEndpoint {
        path: "/insights/history",
        name: "listQueryHistory",
    },
    PaginatedEndpoint {
        path: "/models",
        name: "listModels",
    },
    PaginatedEndpoint {
        path: "/runs/run-1/stages/detect-drift/turns",
        name: "listStageTurns",
    },
    PaginatedEndpoint {
        path: "/runs/run-1/questions",
        name: "listRunQuestions",
    },
    PaginatedEndpoint {
        path: "/runs/run-1/stages",
        name: "listRunStages",
    },
    PaginatedEndpoint {
        path: "/runs/run-1/verification",
        name: "retrieveRunVerification",
    },
];

#[tokio::test]
async fn paginated_endpoints_return_correct_shape() {
    let state = create_app_state(test_db().await, test_llm_spec);
    let app = build_router(state, AuthMode::Disabled);

    for ep in ENDPOINTS {
        // Default request: paginated shape, has_more = false (fixtures fit in default page)
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
