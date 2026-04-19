use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_config::parse_settings_layer;
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::build_router;
use serde_json::json;
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

[server.auth]
methods = ["dev-token", "github"]

[server.auth.github]
allowed_usernames = ["alice"]

[server.storage]
root = "{}"

[server.scheduler]
max_concurrent_runs = 9

[server.integrations.github]
app_id = "{{{{ env.GITHUB_APP_ID }}}}"
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
        "{{ env.GITHUB_APP_ID }}"
    );
    assert_eq!(
        body["server"]["integrations"]["github"]["client_id"],
        "Iv1.github"
    );
    assert_eq!(
        body["server"]["auth"]["methods"],
        json!(["dev-token", "github"])
    );
    assert_eq!(
        body["server"]["auth"]["github"]["allowed_usernames"],
        json!(["alice"])
    );
    assert!(body.pointer("/server/listen").is_none());
}
