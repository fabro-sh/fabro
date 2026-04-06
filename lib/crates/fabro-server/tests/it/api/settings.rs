use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::{build_router, create_app_state_with_options};
use fabro_types::Settings;
use tower::ServiceExt;

use crate::helpers::body_json;

#[tokio::test]
async fn retrieve_server_settings_returns_runtime_settings() {
    let settings: Settings = toml::from_str(
        r#"
storage_dir = "/srv/fabro"
max_concurrent_runs = 9
verbose = true

[vars]
server_only = "1"
"#,
    )
    .expect("settings fixture should parse");
    let app = build_router(
        create_app_state_with_options(settings, 5),
        AuthMode::Disabled,
    );

    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/settings")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response.into_body()).await;
    assert_eq!(body["storage_dir"], "/srv/fabro");
    assert_eq!(body["max_concurrent_runs"], 9);
    assert_eq!(body["verbose"], true);
    assert_eq!(body["vars"]["server_only"], "1");
}
