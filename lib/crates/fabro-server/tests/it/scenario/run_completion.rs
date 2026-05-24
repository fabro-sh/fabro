use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_auth::EnvCredentialSource;
use fabro_model::{Catalog, ProviderId};
use fabro_test::{TwinScenario, TwinScenarios, twin_openai};
use fabro_types::RunId;
use tokio::time::sleep;
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, checked_response, create_and_start_run_from_manifest, minimal_manifest_json,
    minimal_manifest_json_with_dry_run, response_text, test_app_state_with_options,
    test_app_with_scheduler, test_settings, wait_for_run_status,
};

const OPENAI_AGENT_MODEL: &str = "gpt-5.4";

const PROJECT_SKILL_AGENT_DOT: &str = r#"digraph ProjectSkillAgent {
    graph [goal="Verify project skills are visible to agent runs"]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    work [shape=box, label="Work", prompt="Respond with done."]

    start -> work -> exit
}"#;

fn test_app_with_openai_agent_backend(openai_base_url: String, api_key: String) -> axum::Router {
    let settings = test_settings();
    let llm_catalog_settings =
        fabro_server::test_support::llm_catalog_settings_with_provider_base_url(
            "openai",
            openai_base_url,
        );
    let catalog = Arc::new(
        Catalog::from_builtin_with_overrides(&llm_catalog_settings)
            .expect("test catalog should build"),
    );
    let source_api_key = api_key.clone();
    let env_api_key = api_key;
    let llm_source: Arc<dyn fabro_auth::CredentialSource> = Arc::new(
        EnvCredentialSource::with_env_lookup(Arc::new(move |name| match name {
            "OPENAI_API_KEY" => Some(source_api_key.clone()),
            _ => None,
        })),
    );
    let state = fabro_server::test_support::TestAppStateBuilder::new()
        .runtime_settings(settings.server_settings, settings.manifest_run_defaults)
        .max_concurrent_runs(5)
        .llm_catalog_settings(llm_catalog_settings)
        .registry_factory(move |interviewer| {
            let catalog = Arc::clone(&catalog);
            let llm_source = Arc::clone(&llm_source);
            let emitter = Arc::new(fabro_workflow::event::Emitter::new(RunId::new()));
            let steering_hub = Arc::new(fabro_workflow::SteeringHub::new(emitter));
            fabro_workflow::handler::default_registry(interviewer, move || {
                Some(Box::new(
                    fabro_workflow::handler::llm::AgentApiBackend::new_with_catalog(
                        OPENAI_AGENT_MODEL.to_string(),
                        ProviderId::openai(),
                        Vec::new(),
                        Arc::clone(&llm_source),
                        Arc::clone(&steering_hub),
                        Arc::clone(&catalog),
                    ),
                ))
            })
        })
        .env_lookup(move |name| match name {
            "OPENAI_API_KEY" => Some(env_api_key.clone()),
            _ => None,
        })
        .build();
    test_app_with_scheduler(state)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_completes_and_status_is_completed() {
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id =
        create_and_start_run_from_manifest(&app, minimal_manifest_json_with_dry_run(MINIMAL_DOT))
            .await;

    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(status, "succeeded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_run_includes_project_skills_from_local_sandbox_working_directory() {
    let project = tempfile::tempdir().expect("project tempdir should create");
    let skill_dir = project
        .path()
        .join(".fabro")
        .join("skills")
        .join("local-server-project-skill");
    tokio::fs::create_dir_all(&skill_dir)
        .await
        .expect("project skill dir should create");
    tokio::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: local-server-project-skill\ndescription: Project-only skill\n---\nUse the project skill.\n",
    )
    .await
    .expect("project skill should write");

    let twin = twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    TwinScenarios::new(&namespace)
        .scenario(
            TwinScenario::responses(OPENAI_AGENT_MODEL)
                .stream(true)
                .text("Done"),
        )
        .load(twin)
        .await;
    let app = test_app_with_openai_agent_backend(twin.base_url.clone(), namespace.clone());

    let mut manifest = minimal_manifest_json(PROJECT_SKILL_AGENT_DOT);
    manifest["title"] = serde_json::Value::String("Project skill agent".to_string());
    manifest["cwd"] = serde_json::Value::String(project.path().display().to_string());
    let run_id = create_and_start_run_from_manifest(&app, manifest).await;

    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(status, "succeeded");
    let logs = twin.request_logs(&namespace).await;
    let requests = logs["requests"]
        .as_array()
        .expect("twin-openai request logs should be an array");
    let instructions = requests
        .iter()
        .find(|request| request["model"] == OPENAI_AGENT_MODEL)
        .and_then(|request| request["instructions_text"].as_str())
        .unwrap_or_default();
    assert!(
        instructions.contains("local-server-project-skill"),
        "expected project skill name in OpenAI instructions, got logs: {logs}"
    );
    assert!(
        instructions.contains("Project-only skill"),
        "expected project skill description in OpenAI instructions, got logs: {logs}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_run_events_returns_sse_stream() {
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id =
        create_and_start_run_from_manifest(&app, minimal_manifest_json_with_dry_run(MINIMAL_DOT))
            .await;

    // Wait for scheduler to promote run.
    sleep(std::time::Duration::from_millis(100)).await;

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/attach")))
        .body(Body::empty())
        .unwrap();

    let response = checked_response(
        app.oneshot(req).await.unwrap(),
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/attach"),
    )
    .await;
    let content_type = response
        .headers()
        .get("content-type")
        .expect("content-type header should be present")
        .to_str()
        .unwrap();
    assert!(
        content_type.contains("text/event-stream"),
        "expected text/event-stream, got: {content_type}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_run_events_replays_terminal_event_after_completion() {
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id =
        create_and_start_run_from_manifest(&app, minimal_manifest_json_with_dry_run(MINIMAL_DOT))
            .await;
    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(status, "succeeded");

    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/attach?since_seq=1")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_text(
        response,
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/attach?since_seq=1"),
    )
    .await;
    let event_names = body
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        .filter_map(|event| event["event"].as_str().map(ToString::to_string))
        .collect::<Vec<_>>();

    assert!(
        event_names.iter().any(|event| event == "run.completed"),
        "expected a replayed terminal event, got {event_names:?}"
    );
    assert_eq!(
        event_names.last().map(String::as_str),
        Some("run.completed")
    );
}
