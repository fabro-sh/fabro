use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_config::parse_settings_layer;
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::build_router;
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, body_json, minimal_manifest_json, test_app_state_with_options,
};

#[tokio::test]
async fn retrieve_run_settings_preserves_templates_and_redacts_sensitive_fields() {
    let storage_dir = tempfile::tempdir().unwrap();
    let settings = parse_settings_layer(&format!(
        r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:32276"

[server.listen.tls]
cert = "/etc/fabro/tls/cert.pem"
key = "/etc/fabro/tls/key.pem"
ca = "/etc/fabro/tls/ca.pem"

[server.auth.api.jwt]
enabled = true
issuer = "https://auth.example.com"
audience = "${{env.JWT_AUDIENCE}}"

[server.auth.api.mtls]
enabled = true
ca = "/etc/fabro/tls/ca.pem"

[server.auth.web.providers.github]
enabled = true
client_id = "Iv1.abcdef"
client_secret = "${{env.GITHUB_OAUTH_SECRET}}"

[server.storage]
root = "{}"

[server.scheduler]
max_concurrent_runs = 9

[server.integrations.github]
app_id = "${{env.GITHUB_APP_ID}}"
client_id = "Iv1.github"
slug = "fabro-app"
"#,
        storage_dir.path().display()
    ))
    .expect("settings fixture should parse");

    let app = build_router(test_app_state_with_options(settings, 5), AuthMode::Disabled);
    let mut manifest = minimal_manifest_json(MINIMAL_DOT);
    manifest["configs"] = serde_json::json!([{
        "type": "user",
        "path": "/tmp/home/.fabro/settings.toml",
        "source": r#"
_version = 1

[run]
goal = "Ship it"

[cli.output]
verbosity = "verbose"

[features]
session_sandboxes = true
"#
    }]);

    let create_request = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&manifest).unwrap()))
        .unwrap();
    let create_response = app.clone().oneshot(create_request).await.unwrap();
    let create_status = create_response.status();
    let create_body = body_json(create_response.into_body()).await;
    assert_eq!(create_status, StatusCode::CREATED, "{create_body}");
    let run_id = create_body["id"]
        .as_str()
        .expect("run ID should be present");

    let get_request = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/settings")))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(get_request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response.into_body()).await;
    assert_eq!(body["_version"], 1);
    assert_eq!(body["run"]["goal"], "Ship it");
    assert_eq!(body["cli"]["output"]["verbosity"], "verbose");
    assert_eq!(body["features"]["session_sandboxes"], true);
    assert_eq!(
        body["server"]["storage"]["root"],
        storage_dir.path().display().to_string()
    );
    assert_eq!(body["server"]["scheduler"]["max_concurrent_runs"], 9);
    assert_eq!(
        body["server"]["integrations"]["github"]["app_id"],
        "${env.GITHUB_APP_ID}"
    );
    assert_eq!(
        body["server"]["auth"]["api"]["jwt"]["enabled"],
        serde_json::json!(true)
    );
    assert_eq!(
        body["server"]["auth"]["api"]["mtls"]["enabled"],
        serde_json::json!(true)
    );
    assert_eq!(
        body["server"]["auth"]["web"]["providers"]["github"]["client_id"],
        "Iv1.abcdef"
    );
    assert!(body.pointer("/server/listen").is_none());
    assert!(body.pointer("/server/auth/api/jwt/issuer").is_none());
    assert!(body.pointer("/server/auth/api/jwt/audience").is_none());
    assert!(body.pointer("/server/auth/api/mtls/ca").is_none());
    assert!(
        body.pointer("/server/auth/web/providers/github/client_secret")
            .is_none()
    );
}
