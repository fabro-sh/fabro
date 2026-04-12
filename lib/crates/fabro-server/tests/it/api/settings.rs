use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_config::parse_settings_layer;
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::{build_router, create_app_state_with_options};
use fabro_types::settings::SettingsLayer;
use tower::ServiceExt;

use crate::helpers::body_json;

#[tokio::test]
async fn retrieve_server_settings_returns_runtime_settings() {
    let settings: SettingsLayer = parse_settings_layer(
        r#"
_version = 1

[server.storage]
root = "/srv/fabro"

[server.scheduler]
max_concurrent_runs = 9

[cli.output]
verbosity = "verbose"

[run.inputs]
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
    // `/api/v1/settings` emits the v2 SettingsLayer shape directly now.
    // Stage 6.6 will replace this with an explicit allow-list DTO.
    assert_eq!(body["server"]["storage"]["root"], "/srv/fabro");
    assert_eq!(body["server"]["scheduler"]["max_concurrent_runs"], 9);
    assert_eq!(body["cli"]["output"]["verbosity"], "verbose");
    assert_eq!(body["run"]["inputs"]["server_only"], "1");
}
