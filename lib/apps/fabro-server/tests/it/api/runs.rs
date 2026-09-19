use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_llm::lithos_catalog::CatalogProvider;
use fabro_types::SandboxProviderKind;
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, body_json, minimal_intent_json, response_json, response_status,
    settings_from_toml, test_app_state_with_options,
};

async fn create_run(app: &axum::Router, intent: serde_json::Value) -> serde_json::Value {
    let request = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&intent).expect("intent should serialize"),
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

async fn daytona_intent(app: &axum::Router) -> serde_json::Value {
    let workspace = tempfile::tempdir().expect("run target workspace should be created");
    let mut intent = minimal_intent_json(app, MINIMAL_DOT, workspace.path()).await;
    intent["environment_id"] = serde_json::json!("default");
    intent["target"] = serde_json::json!({ "kind": "none" });
    intent
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

fn daytona_disabled_app() -> (axum::Router, tempfile::TempDir) {
    let temp_dir = tempfile::tempdir().expect("daytona disabled test tempdir should be created");
    let active_config_path = temp_dir.path().join("settings.toml");
    let settings = daytona_disabled_settings();
    let state = fabro_server::test_support::TestAppStateBuilder::new()
        .runtime_settings(settings.server_settings, settings.manifest_run_defaults)
        .active_config_path(active_config_path)
        .default_environment_provider(Some(SandboxProviderKind::DAYTONA))
        .build();
    (
        fabro_server::test_support::build_test_router(state),
        temp_dir,
    )
}

#[tokio::test]
async fn create_run_rejects_disabled_sandbox_provider() {
    let (app, _temp_dir) = daytona_disabled_app();

    let request = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(daytona_intent(&app).await.to_string()))
        .expect("create run request should build");
    let body = response_json(
        app.clone().oneshot(request).await.unwrap(),
        StatusCode::SERVICE_UNAVAILABLE,
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
    let (app, _temp_dir) = daytona_disabled_app();

    let request = Request::builder()
        .method("POST")
        .uri(api("/preflight"))
        .header("content-type", "application/json")
        .body(Body::from(
            crate::helpers::minimal_manifest_json(MINIMAL_DOT).to_string(),
        ))
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
    let workspace = tempfile::tempdir().expect("run target workspace should be created");
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
    let created = create_run(
        &app,
        minimal_intent_json(&app, MINIMAL_DOT, workspace.path()).await,
    )
    .await;
    let run_id = created["id"].as_str().unwrap();

    assert_eq!(created["ask_fabro"]["available"], false);
    assert_eq!(
        created["ask_fabro"]["unavailable_reason"],
        "sandbox_not_ready"
    );
    let catalog = fabro_llm::test_support::test_catalog();
    let default_openai_model = catalog
        .enabled_provider("openai")
        .and_then(CatalogProvider::default_offering)
        .expect("the built-in OpenAI provider should have a default model");
    assert_eq!(
        created["ask_fabro"]["default_model"].as_str(),
        Some(default_openai_model.model.id().as_str())
    );

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
    let workspace = tempfile::tempdir().expect("run target workspace should be created");
    let storage_dir = tempfile::tempdir().expect("run target workspace should be created");
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
    let mut intent = minimal_intent_json(&app, MINIMAL_DOT, workspace.path()).await;
    intent["goal"] = serde_json::json!("Ship it");

    let create_request = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&intent).unwrap()))
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
    let workspace = tempfile::tempdir().expect("run target workspace should be created");
    let app = fabro_server::test_support::build_test_router(crate::helpers::test_app_state());
    let parent = create_run(
        &app,
        minimal_intent_json(&app, MINIMAL_DOT, workspace.path()).await,
    )
    .await;
    let parent_id = parent["id"].as_str().unwrap();
    let mut child_intent = minimal_intent_json(&app, MINIMAL_DOT, workspace.path()).await;
    child_intent["parent_id"] = serde_json::json!(parent_id);

    let child = create_run(&app, child_intent).await;
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
    let workspace = tempfile::tempdir().expect("run target workspace should be created");
    let app = fabro_server::test_support::build_test_router(crate::helpers::test_app_state());
    let parent_1 = create_run(
        &app,
        minimal_intent_json(&app, MINIMAL_DOT, workspace.path()).await,
    )
    .await;
    let parent_2 = create_run(
        &app,
        minimal_intent_json(&app, MINIMAL_DOT, workspace.path()).await,
    )
    .await;
    let child = create_run(
        &app,
        minimal_intent_json(&app, MINIMAL_DOT, workspace.path()).await,
    )
    .await;
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
    // The run's stream holds Fabro's platform records: the run's creation,
    // its submission, and one `run.parent` record per link.
    let record_kinds = events["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["kind"] == "platform")
        .map(|item| item["item"]["record"]["kind"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(record_kinds, vec![
        "run.created",
        "run.lifecycle",
        "run.parent",
        "run.parent"
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
    let workspace = tempfile::tempdir().expect("run target workspace should be created");
    let app = fabro_server::test_support::build_test_router(crate::helpers::test_app_state());
    let parent = create_run(
        &app,
        minimal_intent_json(&app, MINIMAL_DOT, workspace.path()).await,
    )
    .await;
    let parent_id = parent["id"].as_str().unwrap();
    let mut child_intent = minimal_intent_json(&app, MINIMAL_DOT, workspace.path()).await;
    child_intent["parent_id"] = serde_json::json!(parent_id);
    let child = create_run(&app, child_intent).await;
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
    let workspace = tempfile::tempdir().expect("run target workspace should be created");
    let app = fabro_server::test_support::build_test_router(crate::helpers::test_app_state());
    let parent = create_run(
        &app,
        minimal_intent_json(&app, MINIMAL_DOT, workspace.path()).await,
    )
    .await;
    let child = create_run(
        &app,
        minimal_intent_json(&app, MINIMAL_DOT, workspace.path()).await,
    )
    .await;
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
