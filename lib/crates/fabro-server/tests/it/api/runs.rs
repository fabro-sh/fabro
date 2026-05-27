use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, body_json, minimal_manifest_json, response_json, response_status,
    settings_from_toml, test_app_state_with_options,
};

async fn create_run(app: &axum::Router, mut manifest: serde_json::Value) -> serde_json::Value {
    manifest
        .as_object_mut()
        .expect("manifest should be an object")
        .entry("configs")
        .or_insert_with(|| serde_json::json!([]));
    let request = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&manifest).expect("manifest should serialize"),
        ))
        .expect("create run request should build");
    response_json(
        app.clone().oneshot(request).await.unwrap(),
        StatusCode::CREATED,
        "POST /api/v1/runs",
    )
    .await
}

async fn request_json(
    app: &axum::Router,
    method: &str,
    path: String,
    body: serde_json::Value,
    expected: StatusCode,
) -> serde_json::Value {
    let request = Request::builder()
        .method(method)
        .uri(api(&path))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&body).expect("request body should serialize"),
        ))
        .expect("JSON request should build");
    response_json(
        app.clone().oneshot(request).await.unwrap(),
        expected,
        format!("{method} /api/v1{path}"),
    )
    .await
}

fn daytona_manifest() -> serde_json::Value {
    let mut manifest = minimal_manifest_json(MINIMAL_DOT);
    manifest["args"] = serde_json::json!({ "environment": "daytona" });
    manifest
}

fn daytona_disabled_settings() -> crate::helpers::TestAppSettings {
    settings_from_toml(
        r"
_version = 1

[server.sandbox.providers.daytona]
enabled = false
",
    )
}

#[tokio::test]
async fn create_run_rejects_disabled_sandbox_provider() {
    let app = fabro_server::test_support::build_test_router(test_app_state_with_options(
        daytona_disabled_settings(),
        5,
    ));

    let request = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(daytona_manifest().to_string()))
        .expect("create run request should build");
    let body = response_json(
        app.clone().oneshot(request).await.unwrap(),
        StatusCode::BAD_REQUEST,
        "POST /api/v1/runs",
    )
    .await;

    assert_eq!(
        body["errors"][0]["detail"],
        "sandbox provider \"daytona\" is disabled by server.sandbox.providers.daytona.enabled"
    );
}

#[tokio::test]
async fn preflight_reports_disabled_sandbox_provider() {
    let app = fabro_server::test_support::build_test_router(test_app_state_with_options(
        daytona_disabled_settings(),
        5,
    ));

    let request = Request::builder()
        .method("POST")
        .uri(api("/preflight"))
        .header("content-type", "application/json")
        .body(Body::from(daytona_manifest().to_string()))
        .expect("preflight request should build");
    let body = response_json(
        app.clone().oneshot(request).await.unwrap(),
        StatusCode::OK,
        "POST /api/v1/preflight",
    )
    .await;

    assert_eq!(body["ok"], false);
    let checks = body["checks"]["sections"][0]["checks"]
        .as_array()
        .expect("preflight checks should be an array");
    let policy_check = checks
        .iter()
        .find(|check| check["name"] == "Sandbox Provider Policy")
        .expect("policy check should be present");
    assert_eq!(policy_check["status"], "error");
    assert_eq!(
        policy_check["summary"],
        "sandbox provider \"daytona\" is disabled by server.sandbox.providers.daytona.enabled"
    );
}

#[tokio::test]
async fn run_responses_include_ask_fabro_affordance() {
    let settings = settings_from_toml(
        r"
_version = 1
",
    );
    let state = fabro_server::test_support::TestAppStateBuilder::new()
        .runtime_settings(settings.server_settings, settings.manifest_run_defaults)
        .vault_entries([("OPENAI_API_KEY", "test-key")])
        .build();
    let app = fabro_server::test_support::build_test_router(state);
    let created = create_run(&app, minimal_manifest_json(MINIMAL_DOT)).await;
    let run_id = created["id"].as_str().unwrap();

    assert_eq!(created["ask_fabro"]["available"], false);
    assert_eq!(
        created["ask_fabro"]["unavailable_reason"],
        "sandbox_not_ready"
    );
    assert_eq!(created["ask_fabro"]["default_model"], "gpt-5.5");

    let get_request = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .unwrap();
    let fetched = response_json(
        app.clone().oneshot(get_request).await.unwrap(),
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}"),
    )
    .await;
    assert_eq!(fetched["ask_fabro"], created["ask_fabro"]);

    let list_request = Request::builder()
        .method("GET")
        .uri(api("/runs"))
        .body(Body::empty())
        .unwrap();
    let list = response_json(
        app.clone().oneshot(list_request).await.unwrap(),
        StatusCode::OK,
        "GET /api/v1/runs",
    )
    .await;
    assert_eq!(list["data"][0]["ask_fabro"], created["ask_fabro"]);
}

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
    assert!(body["project"].get("directory").is_none());
    assert_eq!(body["workflow"]["graph"], "workflow.fabro");
    assert_eq!(body["run"]["goal"]["type"], "inline");
    assert_eq!(body["run"]["goal"]["value"], "Ship it");
    assert!(body.pointer("/_version").is_none());
    assert!(body.pointer("/cli").is_none());
    assert!(body.pointer("/features").is_none());
    assert!(body.pointer("/server").is_none());
}

#[tokio::test]
async fn create_run_can_set_parent_and_list_children() {
    let app = fabro_server::test_support::build_test_router(crate::helpers::test_app_state());
    let parent = create_run(&app, minimal_manifest_json(MINIMAL_DOT)).await;
    let parent_id = parent["id"].as_str().unwrap();
    let mut child_manifest = minimal_manifest_json(MINIMAL_DOT);
    child_manifest["parent_id"] = serde_json::json!(parent_id);

    let child = create_run(&app, child_manifest).await;
    let child_id = child["id"].as_str().unwrap();

    assert_eq!(child["parent_id"], parent_id);
    let list_request = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs?parent_id={parent_id}")))
        .body(Body::empty())
        .unwrap();
    let list = response_json(
        app.clone().oneshot(list_request).await.unwrap(),
        StatusCode::OK,
        "GET /api/v1/runs?parent_id",
    )
    .await;
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
    assert_eq!(list["data"][0]["id"], child_id);
}

#[tokio::test]
async fn link_relink_and_unlink_parent_are_idempotent() {
    let app = fabro_server::test_support::build_test_router(crate::helpers::test_app_state());
    let parent_1 = create_run(&app, minimal_manifest_json(MINIMAL_DOT)).await;
    let parent_2 = create_run(&app, minimal_manifest_json(MINIMAL_DOT)).await;
    let child = create_run(&app, minimal_manifest_json(MINIMAL_DOT)).await;
    let parent_1_id = parent_1["id"].as_str().unwrap();
    let parent_2_id = parent_2["id"].as_str().unwrap();
    let child_id = child["id"].as_str().unwrap();

    let linked = request_json(
        &app,
        "PUT",
        format!("/runs/{child_id}/parent"),
        serde_json::json!({ "parent_id": parent_1_id }),
        StatusCode::OK,
    )
    .await;
    assert_eq!(linked["parent_id"], parent_1_id);

    let linked_again = request_json(
        &app,
        "PUT",
        format!("/runs/{child_id}/parent"),
        serde_json::json!({ "parent_id": parent_1_id }),
        StatusCode::OK,
    )
    .await;
    assert_eq!(linked_again["parent_id"], parent_1_id);

    let relinked = request_json(
        &app,
        "PUT",
        format!("/runs/{child_id}/parent"),
        serde_json::json!({ "parent_id": parent_2_id }),
        StatusCode::OK,
    )
    .await;
    assert_eq!(relinked["parent_id"], parent_2_id);

    let events_request = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{child_id}/events")))
        .body(Body::empty())
        .unwrap();
    let events = response_json(
        app.clone().oneshot(events_request).await.unwrap(),
        StatusCode::OK,
        format!("GET /api/v1/runs/{child_id}/events"),
    )
    .await;
    let event_names = events["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["event"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(event_names, vec![
        "run.created",
        "run.submitted",
        "run.parent.linked",
        "run.parent.linked"
    ]);

    let unlink_request = Request::builder()
        .method("DELETE")
        .uri(api(&format!("/runs/{child_id}/parent")))
        .body(Body::empty())
        .unwrap();
    let unlinked = response_json(
        app.clone().oneshot(unlink_request).await.unwrap(),
        StatusCode::OK,
        format!("DELETE /api/v1/runs/{child_id}/parent"),
    )
    .await;
    assert!(unlinked["parent_id"].is_null());

    let unlink_again = Request::builder()
        .method("DELETE")
        .uri(api(&format!("/runs/{child_id}/parent")))
        .body(Body::empty())
        .unwrap();
    let unlinked_again = response_json(
        app.clone().oneshot(unlink_again).await.unwrap(),
        StatusCode::OK,
        format!("DELETE /api/v1/runs/{child_id}/parent"),
    )
    .await;
    assert!(unlinked_again["parent_id"].is_null());
}

#[tokio::test]
async fn deleting_parent_leaves_child_parent_id_as_historical_reference() {
    let app = fabro_server::test_support::build_test_router(crate::helpers::test_app_state());
    let parent = create_run(&app, minimal_manifest_json(MINIMAL_DOT)).await;
    let parent_id = parent["id"].as_str().unwrap();
    let mut child_manifest = minimal_manifest_json(MINIMAL_DOT);
    child_manifest["parent_id"] = serde_json::json!(parent_id);
    let child = create_run(&app, child_manifest).await;
    let child_id = child["id"].as_str().unwrap();

    let delete_request = Request::builder()
        .method("DELETE")
        .uri(api(&format!("/runs/{parent_id}?force=true")))
        .body(Body::empty())
        .unwrap();
    crate::helpers::checked_response_in(
        app.clone().oneshot(delete_request).await.unwrap(),
        &[StatusCode::OK, StatusCode::NO_CONTENT],
        format!("DELETE /api/v1/runs/{parent_id}?force=true"),
    )
    .await;

    let get_child = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{child_id}")))
        .body(Body::empty())
        .unwrap();
    let child_after_delete = response_json(
        app.clone().oneshot(get_child).await.unwrap(),
        StatusCode::OK,
        format!("GET /api/v1/runs/{child_id}"),
    )
    .await;
    assert_eq!(child_after_delete["parent_id"], parent_id);

    request_json(
        &app,
        "PUT",
        format!("/runs/{child_id}/parent"),
        serde_json::json!({ "parent_id": parent_id }),
        StatusCode::NOT_FOUND,
    )
    .await;
}

#[tokio::test]
async fn parent_link_validation_rejects_missing_self_and_cycles() {
    let app = fabro_server::test_support::build_test_router(crate::helpers::test_app_state());
    let parent = create_run(&app, minimal_manifest_json(MINIMAL_DOT)).await;
    let child = create_run(&app, minimal_manifest_json(MINIMAL_DOT)).await;
    let parent_id = parent["id"].as_str().unwrap();
    let child_id = child["id"].as_str().unwrap();

    request_json(
        &app,
        "PUT",
        format!("/runs/{child_id}/parent"),
        serde_json::json!({ "parent_id": parent_id }),
        StatusCode::OK,
    )
    .await;

    request_json(
        &app,
        "PUT",
        format!("/runs/{parent_id}/parent"),
        serde_json::json!({ "parent_id": child_id }),
        StatusCode::BAD_REQUEST,
    )
    .await;
    request_json(
        &app,
        "PUT",
        format!("/runs/{child_id}/parent"),
        serde_json::json!({ "parent_id": child_id }),
        StatusCode::BAD_REQUEST,
    )
    .await;
    request_json(
        &app,
        "PUT",
        format!("/runs/{child_id}/parent"),
        serde_json::json!({ "parent_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV" }),
        StatusCode::NOT_FOUND,
    )
    .await;

    let missing_child = Request::builder()
        .method("DELETE")
        .uri(api("/runs/01ARZ3NDEKTSV4RRFFQ69G5FAV/parent"))
        .body(Body::empty())
        .unwrap();
    response_status(
        app.oneshot(missing_child).await.unwrap(),
        StatusCode::NOT_FOUND,
        "DELETE /api/v1/runs/{missing}/parent",
    )
    .await;
}
