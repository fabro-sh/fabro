use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, minimal_manifest_json, response_json, response_status, test_app_state,
};

#[tokio::test]
async fn vnc_for_missing_run_returns_not_found() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let fake = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{fake}/sandbox/vnc")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();

    response_status(
        response,
        StatusCode::NOT_FOUND,
        "POST /api/v1/runs/{id}/sandbox/vnc",
    )
    .await;
}

#[tokio::test]
async fn vnc_for_run_without_sandbox_returns_not_found() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let create_req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&minimal_manifest_json(MINIMAL_DOT)).unwrap(),
        ))
        .unwrap();
    let create_response = app.clone().oneshot(create_req).await.unwrap();
    let create_body =
        response_json(create_response, StatusCode::CREATED, "POST /api/v1/runs").await;
    let run_id = create_body["id"].as_str().unwrap();
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/sandbox/vnc")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();

    response_status(
        response,
        StatusCode::NOT_FOUND,
        format!("POST /api/v1/runs/{run_id}/sandbox/vnc"),
    )
    .await;
}
