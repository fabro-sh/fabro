use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_server::install::{InstallAppState, build_install_router};
use tower::ServiceExt;

use crate::helpers::response_json;

#[tokio::test]
async fn install_llm_endpoints_reject_openai_compatible_in_v1() {
    let app = build_install_router(InstallAppState::for_test("test-install-token"));

    let test_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/install/llm/test")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"provider":"openai_compatible","api_key":"test-key"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let test_body = response_json(
        test_response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /install/llm/test",
    )
    .await;
    assert_eq!(
        test_body["errors"][0]["detail"],
        "openai_compatible is not supported by install in v1"
    );

    let put_response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/install/llm")
                .header("authorization", "Bearer test-install-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"providers":[{"provider":"openai_compatible","api_key":"test-key"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let put_body = response_json(
        put_response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "PUT /install/llm",
    )
    .await;
    assert_eq!(
        put_body["errors"][0]["detail"],
        "openai_compatible is not supported by install in v1"
    );
}
