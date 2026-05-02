use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, body_json, minimal_manifest_json, response_json, settings_from_toml,
    test_app_state_with_options,
};

#[tokio::test]
async fn retrieve_run_settings_returns_dense_snapshot() {
    let storage_dir = tempfile::tempdir().unwrap();
    let settings = settings_from_toml(&format!(
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
    ));

    let app =
        fabro_server::test_support::build_test_router(test_app_state_with_options(settings, 5));
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

    let body = response_json(
        response,
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/settings"),
    )
    .await;
    assert_eq!(body["project"]["directory"], ".");
    assert_eq!(body["workflow"]["graph"], "workflow.fabro");
    assert_eq!(body["run"]["goal"]["type"], "inline");
    assert_eq!(body["run"]["goal"]["value"], "Ship it");
    assert!(body.pointer("/_version").is_none());
    assert!(body.pointer("/cli").is_none());
    assert!(body.pointer("/features").is_none());
    assert!(body.pointer("/server").is_none());
}
