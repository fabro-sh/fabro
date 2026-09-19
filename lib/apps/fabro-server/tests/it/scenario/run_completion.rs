use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_static::EnvVars;
use fabro_test::{TwinScenario, TwinScenarios, twin_openai};
use tokio::time::sleep;
use tower::ServiceExt;

use crate::helpers::{
    MINIMAL_DOT, api, checked_response, create_and_start_run_from_intent, minimal_intent_json,
    minimal_intent_json_with_dry_run, response_text, test_app_state_with_options,
    test_app_with_scheduler, test_settings, wait_for_run_status,
};

const OPENAI_AGENT_MODEL: &str = "gpt-5.4";

const PROJECT_SKILL_AGENT_DOT: &str = r#"digraph ProjectSkillAgent {
    graph [goal="Verify project skills are visible to agent runs"]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    work [shape=box, label="Work", prompt="Respond with done.", model="gpt-5.4"]

    start -> work -> exit
}"#;

/// A server whose agent stages reach the OpenAI twin through Petri's model
/// client, executing runs in this process.
fn test_app_with_openai_agent_backend(openai_base_url: String, api_key: String) -> axum::Router {
    let settings = test_settings();
    let llm_overlay =
        fabro_server::test_support::llm_overlay_with_provider_base_url("openai", openai_base_url);
    let env_api_key = api_key.clone();
    let state = fabro_server::test_support::TestAppStateBuilder::new()
        .runtime_settings(settings.server_settings, settings.manifest_run_defaults)
        .max_concurrent_runs(5)
        .llm_overlay(llm_overlay)
        .vault_entries([(EnvVars::OPENAI_API_KEY, api_key)])
        .in_process_execution()
        .env_lookup(move |name| match name {
            "OPENAI_API_KEY" => Some(env_api_key.clone()),
            _ => None,
        })
        .build();
    test_app_with_scheduler(state)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_completes_and_status_is_completed() {
    let workspace = tempfile::tempdir().unwrap();
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id = create_and_start_run_from_intent(
        &app,
        minimal_intent_json_with_dry_run(&app, MINIMAL_DOT, workspace.path()).await,
    )
    .await;

    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(status, "succeeded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_run_includes_project_skills_from_local_sandbox_working_directory() {
    let workspace = tempfile::tempdir().unwrap();
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
    // The run's workspace is a clone of the project, so the skill has to be
    // committed there.
    commit_all(project.path());

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

    let mut intent = minimal_intent_json(&app, PROJECT_SKILL_AGENT_DOT, workspace.path()).await;
    intent["title"] = serde_json::Value::String("Project skill agent".to_string());
    intent["target"] = serde_json::json!({"kind": "folder", "path": project.path()});
    let run_id = create_and_start_run_from_intent(&app, intent).await;

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
    let workspace = tempfile::tempdir().unwrap();
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id = create_and_start_run_from_intent(
        &app,
        minimal_intent_json_with_dry_run(&app, MINIMAL_DOT, workspace.path()).await,
    )
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
    let workspace = tempfile::tempdir().unwrap();
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let run_id = create_and_start_run_from_intent(
        &app,
        minimal_intent_json_with_dry_run(&app, MINIMAL_DOT, workspace.path()).await,
    )
    .await;
    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(status, "succeeded");

    // The stream replays from its first item and ends with the terminal
    // lifecycle record Fabro wrote after Petri's own finish.
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/attach?after=0")))
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let body = response_text(
        response,
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/attach?after=0"),
    )
    .await;
    let items = body
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        .collect::<Vec<_>>();
    let names = items
        .iter()
        .map(|item| {
            if item["kind"] == "platform" {
                item["item"]["record"]["kind"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            } else {
                item["item"]["record"]["body"]["event"]
                    .as_str()
                    .or_else(|| item["item"]["derived"]["event"].as_str())
                    .unwrap_or_default()
                    .to_string()
            }
        })
        .collect::<Vec<_>>();
    assert!(
        names.iter().any(|name| name == "run.finished"),
        "expected Petri's finish in the replay, got {names:?}"
    );
    let last = items.last().expect("the replay has items");
    assert_eq!(last["kind"], "platform", "{last}");
    assert_eq!(last["item"]["record"]["kind"], "run.lifecycle", "{last}");
    assert_eq!(last["item"]["record"]["transition"], "succeeded", "{last}");
}

/// Make `path` a git repository with every file committed, so a run whose
/// target is the folder starts from a clone that holds them.
#[expect(
    clippy::disallowed_methods,
    reason = "the fixture commits with the real git CLI, synchronously"
)]
fn commit_all(path: &std::path::Path) {
    for args in [
        vec!["init", "--quiet", "--initial-branch=main"],
        vec!["add", "--all"],
        vec![
            "-c",
            "user.name=Fabro Test",
            "-c",
            "user.email=test@fabro.sh",
            "commit",
            "--quiet",
            "--message",
            "project",
        ],
    ] {
        let output = std::process::Command::new("git")
            .args(&args)
            .current_dir(path)
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
