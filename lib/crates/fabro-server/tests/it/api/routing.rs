use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::{build_router, create_app_state};
use tower::ServiceExt;

use crate::helpers::body_json;

#[tokio::test]
async fn old_unversioned_routes_return_404() {
    let app = build_router(create_app_state(), AuthMode::Disabled);

    let cases = [(Method::POST, "/completions")];

    for (method, path) in cases {
        let req = Request::builder()
            .method(method.clone())
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
    }
}

#[tokio::test]
async fn root_and_health_stay_at_root() {
    let app = build_router(create_app_state(), AuthMode::Disabled);

    let root_req = Request::builder()
        .method("GET")
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let root_response = app.clone().oneshot(root_req).await.unwrap();
    assert_eq!(root_response.status(), StatusCode::OK);
    let root_body = to_bytes(root_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let root_html = String::from_utf8(root_body.to_vec()).unwrap();
    assert!(root_html.contains("<div id=\"root\"></div>"));

    let health_req = Request::builder()
        .method("GET")
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let health_response = app.oneshot(health_req).await.unwrap();
    assert_eq!(health_response.status(), StatusCode::OK);
    let health_body = body_json(health_response.into_body()).await;
    assert_eq!(health_body["status"], "ok");
}

#[tokio::test]
async fn moved_routes_not_at_root_of_api_prefix() {
    let app = build_router(create_app_state(), AuthMode::Disabled);

    for path in ["/api/v1/health", "/api/v1/"] {
        let req = Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "GET {path}");
    }
}
