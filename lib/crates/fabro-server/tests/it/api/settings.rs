use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_config::parse_settings_layer;
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::{build_router, create_app_state_with_options};
use fabro_types::settings::SettingsLayer;
use tower::ServiceExt;

use crate::helpers::body_json;

#[tokio::test]
async fn retrieve_server_settings_default_view_returns_redacted_layer_settings() {
    let settings: SettingsLayer = parse_settings_layer(
        r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:32276"

[server.listen.tls]
cert = "/etc/fabro/tls/cert.pem"
key = "/etc/fabro/tls/key.pem"
ca = "/etc/fabro/tls/ca.pem"

[server.storage]
root = "/srv/fabro"

[server.scheduler]
max_concurrent_runs = 9

[cli.output]
verbosity = "verbose"

[server.auth.api.jwt]
enabled = true
issuer = "https://auth.example.com"
audience = "fabro"

[server.auth.api.mtls]
enabled = true
ca = "/etc/fabro/ca.pem"

[server.auth.web.providers.github]
enabled = true
client_id = "Iv1.abcdef"
client_secret = "{{ env.GITHUB_OAUTH_SECRET }}"

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
    assert_eq!(body["_version"], 1);
    assert_eq!(body["server"]["storage"]["root"], "/srv/fabro");
    assert_eq!(body["server"]["scheduler"]["max_concurrent_runs"], 9);
    assert_eq!(body["cli"]["output"]["verbosity"], "verbose");
    assert_eq!(body["run"]["inputs"]["server_only"], "1");
    assert!(body["server"].get("listen").is_none());
    assert_eq!(body["server"]["auth"]["api"]["jwt"]["enabled"], true);
    assert!(body["server"]["auth"]["api"]["jwt"].get("issuer").is_none());
    assert!(
        body["server"]["auth"]["api"]["jwt"]
            .get("audience")
            .is_none()
    );
    assert_eq!(body["server"]["auth"]["api"]["mtls"]["enabled"], true);
    assert!(body["server"]["auth"]["api"]["mtls"].get("ca").is_none());
    assert_eq!(
        body["server"]["auth"]["web"]["providers"]["github"]["client_id"],
        "Iv1.abcdef"
    );
    assert!(
        body["server"]["auth"]["web"]["providers"]["github"]
            .get("client_secret")
            .is_none()
    );
}

#[tokio::test]
async fn retrieve_server_settings_resolved_view_returns_dense_settings_and_marker() {
    let settings: SettingsLayer = parse_settings_layer(
        r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:32276"

[server.storage]
root = "/srv/fabro"

[server.auth.web.providers.github]
enabled = true
client_id = "Iv1.abcdef"
client_secret = "{{ env.GITHUB_OAUTH_SECRET }}"

[run.model]
provider = "openai"
name = "server-model"

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
        .uri("/api/v1/settings?view=resolved")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-fabro-settings-view")
            .and_then(|value| value.to_str().ok()),
        Some("resolved")
    );
    let body = body_json(response.into_body()).await;
    assert!(body.get("_version").is_none());
    assert_eq!(body["project"]["directory"], ".");
    assert_eq!(body["workflow"]["graph"], "workflow.fabro");
    assert_eq!(body["run"]["execution"]["approval"], "prompt");
    assert_eq!(body["run"]["model"]["provider"], "openai");
    assert_eq!(body["run"]["model"]["name"], "server-model");
    assert_eq!(body["run"]["inputs"]["server_only"], "1");
    assert_eq!(body["server"]["storage"]["root"], "/srv/fabro");
    assert!(body["server"].get("listen").is_none());
    assert_eq!(
        body["server"]["auth"]["web"]["providers"]["github"]["client_id"],
        "Iv1.abcdef"
    );
    assert!(
        body["server"]["auth"]["web"]["providers"]["github"]
            .get("client_secret")
            .is_none()
    );
}
