use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::{build_router, create_app_state_with_runtime_settings_and_options};
use tower::ServiceExt;

use crate::helpers::{response_json, settings_from_toml};

#[tokio::test]
async fn retrieve_server_settings_returns_dense_server_settings_from_app_state() {
    let settings = settings_from_toml(
        r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:32276"

[server.storage]
root = "/srv/fabro"

[server.scheduler]
max_concurrent_runs = 9

[server.auth]
methods = ["dev-token", "github"]

[server.auth.github]
allowed_usernames = ["alice"]

[server.integrations.github]
client_id = "Iv1.abcdef"
"#,
    );
    let app = build_router(
        create_app_state_with_runtime_settings_and_options(
            settings.server_settings,
            settings.manifest_run_defaults,
            5,
        ),
        AuthMode::Disabled,
    );

    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/settings")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();

    let body = response_json(response, StatusCode::OK, "GET /api/v1/settings").await;
    let top_level = body
        .as_object()
        .expect("server settings response should be an object");
    assert_eq!(top_level.len(), 2);
    assert!(top_level.contains_key("server"));
    assert!(top_level.contains_key("features"));

    assert_eq!(body["server"]["listen"]["type"], "tcp");
    assert_eq!(body["server"]["listen"]["address"], "127.0.0.1:32276");
    assert_eq!(body["server"]["storage"]["root"], "/srv/fabro");
    assert_eq!(body["server"]["scheduler"]["max_concurrent_runs"], 9);
    assert_eq!(body["server"]["auth"]["methods"][0], "dev-token");
    assert_eq!(body["server"]["auth"]["methods"][1], "github");
    assert_eq!(
        body["server"]["auth"]["github"]["allowed_usernames"][0],
        "alice"
    );
    assert_eq!(
        body["server"]["integrations"]["github"]["client_id"],
        "Iv1.abcdef"
    );
    assert_eq!(body["features"]["session_sandboxes"], false);
    assert!(body.get("cli").is_none());
    assert!(body.get("run").is_none());
}
