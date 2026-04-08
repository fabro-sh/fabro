use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::{build_router, create_app_state};
use std::path::PathBuf;
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

#[tokio::test]
async fn source_maps_are_not_served() {
    let app = build_router(create_app_state(), AuthMode::Disabled);
    let map_path = find_dist_source_map();

    let request = Request::builder()
        .method("GET")
        .uri(format!("/{}", map_path))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

fn find_dist_source_map() -> String {
    let dist_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../apps/fabro-web/dist");
    let mut entries = std::fs::read_dir(&dist_dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", dist_dir.display()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    entries.sort_by_key(|entry| entry.path());

    entries
        .into_iter()
        .flat_map(walk_entry)
        .find_map(|path| {
            path.strip_prefix(&dist_dir)
                .ok()
                .and_then(|relative| relative.to_str().map(ToOwned::to_owned))
        })
        .unwrap_or_else(|| {
            panic!(
                "expected at least one .map file under {}",
                dist_dir.display()
            )
        })
}

fn walk_entry(entry: std::fs::DirEntry) -> Vec<PathBuf> {
    let path = entry.path();
    if path.is_dir() {
        let mut children = std::fs::read_dir(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        children.sort_by_key(|child| child.path());
        children.into_iter().flat_map(walk_entry).collect()
    } else if path.extension().is_some_and(|extension| extension == "map") {
        vec![path]
    } else {
        Vec::new()
    }
}
