//! Runs on Petri through the server: every run executes on Petri, Petri's
//! record of the run agrees with Fabro's status, and Petri's diagnostics
//! refuse a run at create.
//!
//! The runs here execute in the server process under the handler-registry
//! test override; outside it the scheduler launches a worker for a Petri
//! run, which the CLI's scenario tests cover with the real binary
//! (`lib/apps/fabro-cli/tests/it/scenario/petri.rs`).
//!
//! Built-in Host scopes run in process without a plugin executable.

#![expect(
    clippy::disallowed_methods,
    reason = "the tests inspect backend availability through the process environment"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_petri::engine::{self, RunStatus};
use fabro_petri::petri::{Access, OwnerId, RunKey, RunStore as _};
use fabro_petri::{SqliteRunStore, test_support};
use fabro_server::server::AppState;
use fabro_server::test_support::{
    TestAppStateBuilder, llm_overlay_with_provider_base_url, test_app_db_pool,
    test_register_workflow_version,
};
use fabro_static::EnvVars;
use fabro_store::platform_records::{PlatformRecord, PlatformRecordKind, PlatformRecordStore};
use fabro_test::{TwinScenario, TwinScenarios, twin_openai};
use fabro_types::settings::run::EnvironmentNetworkMode;
use fabro_types::{RunId, WorkflowPath, WorkflowVersion};
use tower::ServiceExt;

use crate::helpers::{
    self, api, create_and_start_run_from_intent, minimal_manifest_json, read_repo_file,
    response_json, run_json, settings_from_toml, test_app_state_with_options,
    test_app_with_scheduler, test_settings, wait_for_run_status,
};

const OPENAI_MODEL: &str = "gpt-5.4";
// A cold Docker daemon may need to download the runner image before the run
// starts.
const DOCKER_RUN_TIMEOUT: Duration = Duration::from_mins(1);

/// A command-only workflow: one script stage between start and exit.
const COMMAND_DOT: &str = r#"digraph Command {
    graph [goal="Run one command"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    say [shape=parallelogram, script="echo hello from petri"]
    start -> say -> exit
}"#;

/// A workflow whose one stage carries an attribute the language does not
/// have.
const UNKNOWN_ATTRIBUTE_DOT: &str = r#"digraph Bad {
    graph [goal="Refuse me"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    work [shape=box, prompt="Do the work", bogus="yes"]
    start -> work -> exit
}"#;

/// A workflow with an edge to a node nobody declared.
const UNDECLARED_NODE_DOT: &str = r#"digraph Bad {
    graph [goal="Refuse me"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    work [shape=box, prompt="Do the work"]
    start -> work -> nowhere -> exit
}"#;

/// A workflow whose stage names a model no catalog has.
const UNKNOWN_MODEL_DOT: &str = r#"digraph Bad {
    graph [goal="Refuse me"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    work [shape=box, prompt="Do the work", model="no-such-model-9000"]
    start -> work -> exit
}"#;

/// Two command branches joined by a fan-in.
const PARALLEL_DOT: &str = r#"digraph Parallel {
    graph [goal="Run two branches"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    fork [shape=component]
    a [shape=parallelogram, script="echo a"]
    b [shape=parallelogram, script="echo b"]
    merge [shape=tripleoctagon]
    start -> fork
    fork -> a
    fork -> b
    a -> merge
    b -> merge
    merge -> exit
}"#;

pub(super) const PLAIN_SETTINGS: &str = "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n";

/// Register a version whose entrypoint is `workflow.fabro`, with the given
/// files beside it.
pub(super) async fn register_version(app: &axum::Router, files: &[(&str, &str)]) -> String {
    let entrypoint = WorkflowPath::new("workflow.fabro").expect("entrypoint path is valid");
    let files = files
        .iter()
        .map(|(path, text)| {
            (
                WorkflowPath::new(*path).expect("fixture path is valid"),
                (*text).to_string(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let version =
        WorkflowVersion::new(entrypoint, files, BTreeMap::new()).expect("fixture version is valid");
    test_register_workflow_version(app, &version, None)
        .await
        .to_string()
}

pub(super) fn intent(version_id: &str, workspace: &std::path::Path) -> serde_json::Value {
    serde_json::json!({
        "workflow_version_id": version_id,
        "target": {"kind": "folder", "path": workspace},
        "environment_id": "local",
        "args": {},
    })
}

/// The `hello` bundle checked into this repository.
fn hello_files() -> [(&'static str, String); 2] {
    let workflow = read_repo_file(".fabro/workflows/hello/workflow.fabro");
    let settings = read_repo_file(".fabro/workflows/hello/workflow.toml");
    [("workflow.fabro", workflow), ("workflow.toml", settings)]
}

/// The run's record in Petri's store, read through the same database the
/// server wrote it to.
async fn petri_outcome(state: &AppState, run_id: &str) -> engine::RunOutcome {
    let store = SqliteRunStore::new(test_app_db_pool(state));
    engine::outcome_of(&store, run_id)
        .await
        .expect("the run's Petri record inspects")
}

/// The run's projected state once its projector settled.
pub(super) async fn settled_state(
    state: &AppState,
    app: &axum::Router,
    run_id: &str,
) -> serde_json::Value {
    let id: RunId = run_id.parse().expect("the run id parses");
    state.test_petri_projector().settle(id).await;
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/state")))
        .body(Body::empty())
        .expect("state request should build");
    let response = app
        .clone()
        .oneshot(req)
        .await
        .expect("state request routes");
    response_json(
        response,
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/state"),
    )
    .await
}

/// How many items the run's projected stream holds.
async fn petri_stream_len(state: &AppState, run_id: &str) -> usize {
    let id: RunId = run_id.parse().expect("the run id parses");
    test_support::stored_stream(&state.test_petri_view_pool(), id)
        .await
        .expect("the stream reads")
        .len()
}

async fn run_admission(app: &axum::Router, run_id: &str) -> serde_json::Value {
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/state")))
        .body(Body::empty())
        .expect("state request should build");
    let response = app
        .clone()
        .oneshot(req)
        .await
        .expect("state request routes");
    let body = response_json(
        response,
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/state"),
    )
    .await;
    body["spec"]["admission"].clone()
}

async fn create_run_response(app: &axum::Router, intent: serde_json::Value) -> serde_json::Value {
    let req = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&intent).expect("intent serializes"),
        ))
        .expect("create-run request should build");
    let response = app
        .clone()
        .oneshot(req)
        .await
        .expect("create request routes");
    response_json(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST /api/v1/runs",
    )
    .await
}

/// The `hello` bundle, whose one stage is a prompt, runs on Petri: the
/// prompt reaches the twin through Petri's model client, Fabro reports the
/// run succeeded, and Petri's record of the run says the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hello_bundle_runs_on_petri() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let twin = twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    TwinScenarios::new(&namespace)
        .scenario(TwinScenario::responses(OPENAI_MODEL).text("A haiku, added."))
        .load(twin)
        .await;
    let settings = test_settings();
    // The handler-registry override is the test switch that keeps a Petri
    // run in this process; without it the scheduler launches a worker.
    let state = TestAppStateBuilder::new()
        .runtime_settings(settings.server_settings, settings.manifest_run_defaults)
        .max_concurrent_runs(5)
        .in_process_execution()
        .llm_overlay(llm_overlay_with_provider_base_url(
            "openai",
            twin.base_url.clone(),
        ))
        .vault_entries([(EnvVars::OPENAI_API_KEY, namespace.clone())])
        .build();
    let app = test_app_with_scheduler(Arc::clone(&state));

    let [(workflow_path, workflow), (settings_path, settings)] = hello_files();
    let version_id = register_version(&app, &[
        (workflow_path, &workflow),
        (settings_path, &settings),
    ])
    .await;
    let mut intent = intent(&version_id, workspace.path());
    intent["args"]["model"] = serde_json::json!(OPENAI_MODEL);
    let run_id = create_and_start_run_from_intent(&app, intent).await;

    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    let run = run_json(&app, &run_id).await;
    assert_eq!(status, "succeeded", "run: {run}");
    assert!(
        run_admission(&app, &run_id).await["graph"]["digest"].is_string(),
        "the run's spec names what Petri admitted"
    );
    let outcome = petri_outcome(&state, &run_id).await;
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(outcome.complete, "{:?}", outcome.incomplete);
    let projection = settled_state(&state, &app, &run_id).await;
    assert_eq!(projection["status"]["kind"], "succeeded", "{projection}");
    assert_eq!(
        projection["conclusion"]["status"], "succeeded",
        "{projection}"
    );
    let greet = &projection["stages"]["greet@1"];
    assert_eq!(greet["state"], "succeeded", "{projection}");
    assert_eq!(greet["handler"], "agent", "{greet}");
    assert!(
        greet["response"]
            .as_str()
            .is_some_and(|response| response.contains("A haiku, added.")),
        "the agent's answer is projected as the stage's response: {greet}"
    );
    assert!(run["usage"]["tokens"]["input"].as_u64().is_some(), "{run}");
    let logs = twin.request_logs(&namespace).await;
    let requests = logs["requests"]
        .as_array()
        .expect("twin request logs are an array");
    assert!(
        requests
            .iter()
            .any(|request| request["model"] == OPENAI_MODEL),
        "the prompt stage should have called the twin, got {logs}"
    );
    super::petri_stream::capture_settled(&state, &app, &run_id, "hello").await;
}

/// A command-only bundle runs on Petri, and Petri's record agrees.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_bundle_runs_on_petri() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let settings = settings_from_toml("_version = 1\n\n[run.environment]\nid = \"local\"\n");
    let state = test_app_state_with_options(settings, 5);
    let app = test_app_with_scheduler(Arc::clone(&state));

    let version_id = register_version(&app, &[
        ("workflow.fabro", COMMAND_DOT),
        ("workflow.toml", PLAIN_SETTINGS),
    ])
    .await;
    let run_id =
        create_and_start_run_from_intent(&app, intent(&version_id, workspace.path())).await;

    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    let run = run_json(&app, &run_id).await;
    assert_eq!(status, "succeeded", "run: {run}");
    assert!(
        run_admission(&app, &run_id).await["graph"]["digest"].is_string(),
        "the run's spec names what Petri admitted"
    );
    let outcome = petri_outcome(&state, &run_id).await;
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(outcome.complete, "{:?}", outcome.incomplete);
    let projection = settled_state(&state, &app, &run_id).await;
    let say = &projection["stages"]["say@1"];
    assert_eq!(say["state"], "succeeded", "{projection}");
    assert_eq!(say["handler"], "command", "{say}");
    assert!(
        say["output"]
            .as_str()
            .is_some_and(|output| output.contains("hello from petri")),
        "{say}"
    );
    let stream = petri_stream_len(&state, &run_id).await;
    assert!(stream > 0, "the run's stream holds its events");
    super::petri_stream::capture_settled(&state, &app, &run_id, "command").await;
}

/// A parallel bundle with two command branches runs on Petri through the
/// server: each branch is a child execution, projected as a stage grouped
/// under the fork, and the fork carries the branch results.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parallel_bundle_projects_its_branches_through_the_server() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let settings = settings_from_toml("_version = 1\n\n[run.environment]\nid = \"local\"\n");
    let state = test_app_state_with_options(settings, 5);
    let app = test_app_with_scheduler(Arc::clone(&state));

    let version_id = register_version(&app, &[
        ("workflow.fabro", PARALLEL_DOT),
        ("workflow.toml", PLAIN_SETTINGS),
    ])
    .await;
    let run_id =
        create_and_start_run_from_intent(&app, intent(&version_id, workspace.path())).await;

    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    let run = run_json(&app, &run_id).await;
    assert_eq!(status, "succeeded", "run: {run}");
    let projection = settled_state(&state, &app, &run_id).await;
    for (branch, index) in [("a@1", 0), ("b@1", 1)] {
        let stage = &projection["stages"][branch];
        assert_eq!(stage["state"], "succeeded", "{branch}: {projection}");
        assert_eq!(
            stage["parallel_branch_id"],
            format!("fork@1:{index}"),
            "{stage}"
        );
    }
    let fork = &projection["stages"]["fork@1"];
    assert_eq!(
        fork["parallel_results"].as_array().map(Vec::len),
        Some(2),
        "{fork}"
    );
    assert_eq!(
        projection["conclusion"]["status"], "succeeded",
        "{projection}"
    );
}

/// A workflow with an attribute the language does not have is refused at
/// create with Petri's code in Fabro's diagnostic shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_attribute_is_refused_at_create_with_petris_code() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let version_id = register_version(&app, &[
        ("workflow.fabro", UNKNOWN_ATTRIBUTE_DOT),
        ("workflow.toml", PLAIN_SETTINGS),
    ])
    .await;
    let body = create_run_response(&app, intent(&version_id, workspace.path())).await;

    let detail = body["errors"][0]["detail"].as_str().unwrap_or_default();
    assert_eq!(body["errors"][0]["code"], "run_compile_invalid", "{body}");
    assert!(
        detail.contains("attractor.unknown_attribute") && detail.contains("bogus"),
        "expected Petri's diagnostic in the detail, got {body}"
    );
}

/// An edge to a node nobody declared is refused at create with Petri's code.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edge_to_an_undeclared_node_is_refused_at_create_with_petris_code() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let version_id = register_version(&app, &[
        ("workflow.fabro", UNDECLARED_NODE_DOT),
        ("workflow.toml", PLAIN_SETTINGS),
    ])
    .await;
    let body = create_run_response(&app, intent(&version_id, workspace.path())).await;

    let detail = body["errors"][0]["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("attractor.undeclared_node") && detail.contains("nowhere"),
        "expected Petri's diagnostic in the detail, got {body}"
    );
}

/// A model selector the catalog cannot resolve is refused at create with
/// `attractor.model.unknown`: Petri's admission pass pins every model
/// against the server's catalog, so nothing is left for a run to discover.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_model_is_refused_at_create_with_attractor_model_unknown() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(state);

    let version_id = register_version(&app, &[
        ("workflow.fabro", UNKNOWN_MODEL_DOT),
        ("workflow.toml", PLAIN_SETTINGS),
    ])
    .await;
    let body = create_run_response(&app, intent(&version_id, workspace.path())).await;

    let detail = body["errors"][0]["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("attractor.model.unknown") && detail.contains("no-such-model-9000"),
        "expected the admission diagnostic in the detail, got {body}"
    );
}

/// A yes/no gate whose branches each leave a marker file.
fn gate_dot(markers: &std::path::Path) -> String {
    format!(
        r#"digraph Gate {{
    graph [goal="Ask before running"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    gate [shape=hexagon, label="Go?", question_type="yes_no"]
    yes [shape=parallelogram, script="touch {dir}/yes"]
    no [shape=parallelogram, script="touch {dir}/no"]
    start -> gate
    gate -> yes [label="[Y] Yes"]
    gate -> no [label="[N] No"]
    yes -> exit
    no -> exit
}}"#,
        dir = markers.display()
    )
}

/// The run's first pending question, once one is listed.
async fn wait_for_question(app: &axum::Router, run_id: &str) -> serde_json::Value {
    for _ in 0..600 {
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/questions")))
            .body(Body::empty())
            .expect("questions request should build");
        let response = app
            .clone()
            .oneshot(req)
            .await
            .expect("questions request routes");
        let body = response_json(
            response,
            StatusCode::OK,
            format!("GET /api/v1/runs/{run_id}/questions"),
        )
        .await;
        if let Some(question) = body["data"].as_array().and_then(|items| items.first()) {
            return question.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("run {run_id} never asked a question");
}

/// A human gate in a Petri run asks through the questions API and is
/// answered through it: the question is listed with the gate's stage and
/// options, the answer routes the gate, and the run's stream records the
/// interview as a legacy stage's would.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_human_gate_is_answered_through_the_questions_api() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let markers = tempfile::tempdir().expect("marker tempdir");
    let settings = settings_from_toml("_version = 1\n\n[run.environment]\nid = \"local\"\n");
    let state = test_app_state_with_options(settings, 5);
    let app = test_app_with_scheduler(Arc::clone(&state));

    let dot = gate_dot(markers.path());
    let version_id = register_version(&app, &[
        ("workflow.fabro", &dot),
        ("workflow.toml", PLAIN_SETTINGS),
    ])
    .await;
    let run_id =
        create_and_start_run_from_intent(&app, intent(&version_id, workspace.path())).await;

    let question = wait_for_question(&app, &run_id).await;
    assert_eq!(question["stage"], "gate@1", "{question}");
    assert_eq!(question["text"], "Go?", "{question}");
    assert_eq!(question["question_type"], "yes_no", "{question}");
    let keys: Vec<&str> = question["options"]
        .as_array()
        .expect("options")
        .iter()
        .filter_map(|option| option["key"].as_str())
        .collect();
    assert_eq!(keys, vec!["Y", "N"], "{question}");
    let question_id = question["id"].as_str().expect("an id").to_string();
    assert!(
        question_id.starts_with("gate#"),
        "Petri's id: {question_id}"
    );

    // Petri's id travels as one percent-encoded path segment, as the
    // generated clients send it.
    let encoded_id =
        percent_encoding::utf8_percent_encode(&question_id, percent_encoding::NON_ALPHANUMERIC)
            .to_string();
    let req = Request::builder()
        .method("POST")
        .uri(api(&format!(
            "/runs/{run_id}/questions/{encoded_id}/answer"
        )))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"kind":"no"}"#))
        .expect("answer request should build");
    let response = app
        .clone()
        .oneshot(req)
        .await
        .expect("answer request routes");
    crate::helpers::response_status(
        response,
        StatusCode::NO_CONTENT,
        format!("POST /api/v1/runs/{run_id}/questions/{question_id}/answer"),
    )
    .await;

    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    let run = run_json(&app, &run_id).await;
    assert_eq!(status, "succeeded", "run: {run}");
    assert!(
        markers.path().join("no").exists() && !markers.path().join("yes").exists(),
        "the no branch ran"
    );
    let outcome = petri_outcome(&state, &run_id).await;
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    let state_body = {
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/state")))
            .body(Body::empty())
            .expect("state request should build");
        response_json(
            app.clone()
                .oneshot(req)
                .await
                .expect("state request routes"),
            StatusCode::OK,
            format!("GET /api/v1/runs/{run_id}/state"),
        )
        .await
    };
    assert!(
        state_body["pending_interviews"]
            .as_object()
            .is_some_and(serde_json::Map::is_empty),
        "the answered question is no longer pending: {}",
        state_body["pending_interviews"]
    );
    // Who answered is a platform record keyed on Petri's id, derived from
    // the adapter's `interview.completed` with the API caller as its actor.
    let answered = PlatformRecordStore::new(state.test_petri_view_pool())
        .read_kind(
            &run_id.parse().expect("the run id parses"),
            PlatformRecordKind::InterviewAnswered,
        )
        .await
        .expect("the platform records read");
    let [answered] = answered.as_slice() else {
        panic!("one question was answered: {answered:?}");
    };
    let PlatformRecord::InterviewAnswered(record) = &answered.record else {
        panic!("an answered record: {answered:?}");
    };
    assert_eq!(record.question, question_id);
    assert!(
        record.principal.is_some(),
        "the answering principal: {record:?}"
    );
    super::petri_stream::capture_settled(&state, &app, &run_id, "gate").await;
}

/// The sandbox a run executed in, as its projection carries it from Petri's
/// `scope.acquired`: the run's own scope on the host provider, ready, with
/// the directory id a reconnect attaches by and the working directory the
/// steps ran in. The summary carries the same instance, so Ask Fabro and
/// `sandbox cp` reach the sandbox after the run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_runs_projection_carries_its_host_sandbox_instance() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let settings = settings_from_toml("_version = 1\n\n[run.environment]\nid = \"local\"\n");
    let state = test_app_state_with_options(settings, 5);
    let app = test_app_with_scheduler(Arc::clone(&state));

    let version_id = register_version(&app, &[
        ("workflow.fabro", COMMAND_DOT),
        ("workflow.toml", PLAIN_SETTINGS),
    ])
    .await;
    let run_id =
        create_and_start_run_from_intent(&app, intent(&version_id, workspace.path())).await;
    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(
        status,
        "succeeded",
        "run: {}",
        run_json(&app, &run_id).await
    );

    let projection = settled_state(&state, &app, &run_id).await;
    let sandbox = &projection["sandbox"];
    assert_eq!(sandbox["kind"], "ready", "{sandbox}");
    assert_eq!(sandbox["plan"]["provider"], "local", "{sandbox}");
    let instance = &sandbox["instance"];
    assert_eq!(instance["provider"], "local", "{instance}");
    assert!(
        instance.get("image").is_none(),
        "a host directory runs no image: {instance}"
    );
    let id = instance["runtime"]["id"]
        .as_str()
        .expect("the provider's id for the sandbox");
    assert!(
        id.starts_with("host-"),
        "the host provider's id for the workspace directory: {id}"
    );
    let working_directory = instance["runtime"]["working_directory"]
        .as_str()
        .expect("the working directory");
    assert!(
        Path::new(working_directory).is_dir(),
        "the workspace is retained after the run: {working_directory}"
    );
    assert!(
        instance["ready_duration_ms"].is_u64(),
        "the acquisition's duration is on the instance: {instance}"
    );
    assert_eq!(
        instance["retained"], true,
        "the release said the sandbox still exists: {instance}"
    );
    assert!(sandbox.get("failure").is_none(), "{sandbox}");

    let run = run_json(&app, &run_id).await;
    assert_eq!(run["sandbox"]["kind"], "ready", "{run}");
    assert_eq!(run["sandbox"]["instance"]["runtime"]["id"], id, "{run}");
}

/// Deleting a run deletes its sandboxes through Petri's lease ledger: a
/// run whose lease a live process holds is refused with a conflict and
/// keeps its workspace, and once the lease is free the delete removes the
/// host workspace Petri kept along with the run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_run_prunes_its_host_workspace_through_petri() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let settings = settings_from_toml("_version = 1\n\n[run.environment]\nid = \"local\"\n");
    let state = test_app_state_with_options(settings, 5);
    let app = test_app_with_scheduler(Arc::clone(&state));

    let version_id = register_version(&app, &[
        ("workflow.fabro", COMMAND_DOT),
        ("workflow.toml", PLAIN_SETTINGS),
    ])
    .await;
    let run_id =
        create_and_start_run_from_intent(&app, intent(&version_id, workspace.path())).await;
    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(
        status,
        "succeeded",
        "run: {}",
        run_json(&app, &run_id).await
    );
    let projection = settled_state(&state, &app, &run_id).await;
    let working_directory = PathBuf::from(
        projection["sandbox"]["instance"]["runtime"]["working_directory"]
            .as_str()
            .expect("the working directory"),
    );
    assert!(
        working_directory.is_dir(),
        "the workspace is retained after the run: {}",
        working_directory.display()
    );

    // The run's lease is free once its execution let go of the record.
    let store = state.test_petri_run_store();
    let key = RunKey::new(run_id.clone());
    wait_for_free_lease(store, &key).await;

    // A live handle on the run, as its worker holds one, refuses the
    // delete: Petri will not prune under a lease someone holds.
    let held = store
        .open(&key, Access::Write {
            owner: OwnerId::new("worker-1"),
        })
        .await
        .expect("the worker takes the run");
    let refused = response_json(
        app.clone()
            .oneshot(delete(&run_id))
            .await
            .expect("delete route"),
        StatusCode::CONFLICT,
        format!("DELETE /api/v1/runs/{run_id}"),
    )
    .await;
    assert!(
        refused["errors"][0]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("held by a live process")),
        "the conflict names the held lease: {refused}"
    );
    assert!(
        working_directory.is_dir(),
        "the refused delete left the workspace"
    );
    drop(held);
    wait_for_free_lease(store, &key).await;

    crate::helpers::response_status(
        app.clone()
            .oneshot(delete(&run_id))
            .await
            .expect("delete route"),
        StatusCode::NO_CONTENT,
        format!("DELETE /api/v1/runs/{run_id}"),
    )
    .await;
    assert!(
        !working_directory.exists(),
        "the host provider removed the workspace Petri kept"
    );
    crate::helpers::response_status(
        app.clone()
            .oneshot(get(&format!("/runs/{run_id}")))
            .await
            .expect("run route"),
        StatusCode::NOT_FOUND,
        format!("GET /api/v1/runs/{run_id}"),
    )
    .await;
}

/// Wait until no owner holds the run's lease.
async fn wait_for_free_lease(store: &SqliteRunStore, key: &RunKey) {
    for _ in 0..500 {
        if store.owner(key).await.expect("reads the lease").is_none() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the run's lease was not released");
}

fn delete(run_id: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(api(&format!("/runs/{run_id}")))
        .body(Body::empty())
        .expect("delete request should build")
}

/// A delete issued the moment the run reads as ended is accepted while its
/// execution still tears down in the server process: the server settles
/// the managed run at Petri's own finish, the record the view ends the run
/// on, not at the terminal record it stores after the engine returns, so
/// the delete precheck does not refuse the run as active. The execution's
/// end after the delete brings nothing back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delete_right_after_the_run_reads_ended_is_accepted() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let settings = settings_from_toml("_version = 1\n\n[run.environment]\nid = \"local\"\n");
    let state = test_app_state_with_options(settings, 5);
    let app = test_app_with_scheduler(Arc::clone(&state));

    let version_id = register_version(&app, &[
        ("workflow.fabro", COMMAND_DOT),
        ("workflow.toml", PLAIN_SETTINGS),
    ])
    .await;
    let run_id =
        create_and_start_run_from_intent(&app, intent(&version_id, workspace.path())).await;
    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(
        status,
        "succeeded",
        "run: {}",
        run_json(&app, &run_id).await
    );
    // The run's lease is free once its execution let go of the record: a
    // delete under a held lease is refused for the lease, which the prune
    // scenario covers, not for the managed run's status.
    let store = state.test_petri_run_store();
    let key = RunKey::new(run_id.clone());
    wait_for_free_lease(store, &key).await;

    let response = app
        .clone()
        .oneshot(delete(&run_id))
        .await
        .expect("delete route");
    let status = response.status();
    let detail = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the delete was refused: {detail}"
    );

    // The execution ends in the background after the delete and must leave
    // the run gone.
    for _ in 0..20 {
        crate::helpers::response_status(
            app.clone()
                .oneshot(get(&format!("/runs/{run_id}")))
                .await
                .expect("run route"),
            StatusCode::NOT_FOUND,
            format!("GET /api/v1/runs/{run_id}"),
        )
        .await;
        assert_eq!(
            state.test_managed_run_status(&run_id.parse().expect("a run id")),
            None,
            "the execution's end brought the managed run back"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// The same on the Docker provider: the instance is the run's container,
/// with the image it runs and the container's workspace, so a reconnect
/// attaches to it on the daemon.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_runs_projection_carries_its_docker_sandbox_instance() {
    if !fabro_test::docker_available() {
        return;
    }
    let settings = settings_from_toml("_version = 1\n\n[run.environment]\nid = \"docker\"\n");
    let state = test_app_state_with_options(settings, 5);
    let app = test_app_with_scheduler(Arc::clone(&state));
    // A Docker environment on the daemon's default runner image.
    let environment = serde_json::json!({
        "id": "docker",
        "provider": "docker",
        "image": { "docker": null, "dockerfile": null },
        "resources": { "cpu": null, "memory": null, "disk": null },
        "network": { "mode": "allow_all", "allow": [] },
        "lifecycle": { "preserve": false, "stop_on_terminal": true, "auto_stop": null },
        "labels": {},
        "env": {}
    });
    let request = Request::builder()
        .method("POST")
        .uri(api("/environments"))
        .header("content-type", "application/json")
        .body(Body::from(environment.to_string()))
        .expect("environment request should build");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("environment request routes");
    response_json(
        response,
        StatusCode::CREATED,
        "POST /api/v1/environments".to_string(),
    )
    .await;

    let version_id = register_version(&app, &[
        ("workflow.fabro", COMMAND_DOT),
        ("workflow.toml", PLAIN_SETTINGS),
    ])
    .await;
    // A Docker environment takes no folder target: the workspace is the
    // container's own.
    let intent = serde_json::json!({
        "workflow_version_id": version_id,
        "target": {"kind": "none"},
        "environment_id": "docker",
        "args": {},
    });
    let run_id = create_and_start_run_from_intent(&app, intent).await;
    let status = helpers::wait_for_run_status_with_timeout(
        &app,
        &run_id,
        &["succeeded", "failed"],
        DOCKER_RUN_TIMEOUT,
    )
    .await;
    assert_eq!(
        status,
        "succeeded",
        "run: {}",
        run_json(&app, &run_id).await
    );

    let projection = settled_state(&state, &app, &run_id).await;
    let sandbox = &projection["sandbox"];
    assert_eq!(sandbox["kind"], "ready", "{sandbox}");
    let instance = &sandbox["instance"];
    assert_eq!(instance["provider"], "docker", "{instance}");
    assert!(
        instance["image"]
            .as_str()
            .is_some_and(|image| !image.is_empty()),
        "the image the container runs: {instance}"
    );
    let id = instance["runtime"]["id"]
        .as_str()
        .expect("the container id");
    assert!(!id.is_empty(), "{instance}");
    assert_eq!(
        instance["runtime"]["working_directory"], "/workspace",
        "{instance}"
    );

    // The container is on the daemon, under Petri's run label.
    let output = Command::new("docker")
        .args([
            "ps",
            "-aq",
            "--filter",
            &format!("label=petri.run={run_id}"),
        ])
        .output()
        .expect("docker ps runs");
    let containers: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    assert!(
        containers
            .iter()
            .any(|container| id.starts_with(container.as_str())),
        "the recorded instance is the run's container: {id} in {containers:?}"
    );
    for container in &containers {
        let _ = Command::new("docker")
            .args(["rm", "-f", container])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// The server reaches the container Petri created, without Petri: the
/// sandbox tab describes it, Run Files lists and round-trips a file in
/// its workspace, a preview URL is opened to a port in it, and an Ask
/// Fabro session runs its turn against it. The container carries Petri's
/// `petri.run` label and none of Fabro's own, so an attach scoped to
/// Fabro's retired labels would refuse it; the server's ownership is the
/// run label.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_server_attaches_to_the_container_petri_created() {
    if !fabro_test::docker_available() {
        return;
    }
    let twin = twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    TwinScenarios::new(&namespace)
        .scenario(TwinScenario::responses(OPENAI_MODEL).text("The workspace is /workspace."))
        .load(twin)
        .await;
    let settings = settings_from_toml("_version = 1\n\n[run.environment]\nid = \"docker\"\n");
    let state = TestAppStateBuilder::new()
        .runtime_settings(settings.server_settings, settings.manifest_run_defaults)
        .max_concurrent_runs(5)
        .in_process_execution()
        .llm_overlay(llm_overlay_with_provider_base_url(
            "openai",
            twin.base_url.clone(),
        ))
        .vault_entries([(EnvVars::OPENAI_API_KEY, namespace.clone())])
        .build();
    let app = test_app_with_scheduler(Arc::clone(&state));
    create_docker_environment(
        &app,
        "docker",
        CATALOG_IMAGE,
        EnvironmentNetworkMode::AllowAll,
    )
    .await;

    let version_id = register_version(&app, &[
        ("workflow.fabro", COMMAND_DOT),
        ("workflow.toml", PLAIN_SETTINGS),
    ])
    .await;
    let intent = serde_json::json!({
        "workflow_version_id": version_id,
        "target": {"kind": "none"},
        "environment_id": "docker",
        "args": {},
    });
    let run_id = create_and_start_run_from_intent(&app, intent).await;
    let status = helpers::wait_for_run_status_with_timeout(
        &app,
        &run_id,
        &["succeeded", "failed"],
        DOCKER_RUN_TIMEOUT,
    )
    .await;
    assert_eq!(
        status,
        "succeeded",
        "run: {}",
        run_json(&app, &run_id).await
    );
    let projection = settled_state(&state, &app, &run_id).await;
    let container = projection["sandbox"]["instance"]["runtime"]["id"]
        .as_str()
        .expect("the container id")
        .to_string();

    // The sandbox tab: the record and the daemon's status for the container.
    let details = response_json(
        app.clone()
            .oneshot(get(&format!("/runs/{run_id}/sandbox")))
            .await
            .expect("sandbox details route"),
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/sandbox"),
    )
    .await;
    assert_eq!(details["sandbox"]["provider"], "docker", "{details}");
    assert_eq!(details["sandbox"]["runtime"]["id"], container, "{details}");
    assert_eq!(details["status"]["id"], container, "{details}");
    assert_eq!(
        details["status"]["labels"]["petri.run"], run_id,
        "the container carries Petri's run label: {details}"
    );
    assert!(
        details["status"]["labels"]
            .as_object()
            .is_some_and(|labels| !labels.contains_key("sh.fabro.managed")),
        "Petri stamps no Fabro label: {details}"
    );

    // Run Files: a file written into the workspace is listed and read back,
    // which starts the container Petri stopped at the run's end.
    let put = Request::builder()
        .method("PUT")
        .uri(api(&format!(
            "/runs/{run_id}/sandbox/file?path=/workspace/from-fabro.txt"
        )))
        .header("content-type", "application/octet-stream")
        .body(Body::from("written through the server"))
        .expect("file upload request should build");
    crate::helpers::response_text(
        app.clone().oneshot(put).await.expect("file upload routes"),
        StatusCode::NO_CONTENT,
        format!("PUT /api/v1/runs/{run_id}/sandbox/file"),
    )
    .await;
    let listing = response_json(
        app.clone()
            .oneshot(get(&format!(
                "/runs/{run_id}/sandbox/files?path=/workspace"
            )))
            .await
            .expect("sandbox files route"),
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/sandbox/files"),
    )
    .await;
    let names: Vec<&str> = listing["data"]
        .as_array()
        .expect("the listing's entries")
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect();
    assert!(
        names.contains(&"from-fabro.txt"),
        "the workspace lists the file: {names:?}"
    );
    let content = crate::helpers::response_text(
        app.clone()
            .oneshot(get(&format!(
                "/runs/{run_id}/sandbox/file?path=/workspace/from-fabro.txt"
            )))
            .await
            .expect("file download route"),
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/sandbox/file"),
    )
    .await;
    assert_eq!(content, "written through the server");

    // A preview URL to a port in the container, through the driver's
    // forward.
    let preview = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/preview")))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "port": 8080, "expires_in_secs": 60, "signed": false }).to_string(),
        ))
        .expect("preview request should build");
    let preview = response_json(
        app.clone().oneshot(preview).await.expect("preview route"),
        StatusCode::CREATED,
        format!("POST /api/v1/runs/{run_id}/preview"),
    )
    .await;
    assert!(
        preview["url"]
            .as_str()
            .is_some_and(|url| url.starts_with("http://")),
        "{preview}"
    );

    // Ask Fabro: the session's turn reconnects to the container (attach,
    // start, the platform probe) before anything else. The in-process app
    // has no daemon record for the run-tools client the turn builds next,
    // so the turn stops there, past the sandbox: a failure the sandbox
    // caused would carry the sandbox code instead.
    let session = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/sessions")))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "title": "Ask Fabro", "model": OPENAI_MODEL }).to_string(),
        ))
        .expect("session request should build");
    let session = response_json(
        app.clone().oneshot(session).await.expect("session route"),
        StatusCode::CREATED,
        format!("POST /api/v1/runs/{run_id}/sessions"),
    )
    .await;
    let session_id = session["id"].as_str().expect("the session id");
    let turn = Request::builder()
        .method("POST")
        .uri(api(&format!("/sessions/{session_id}/turns")))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"input":"Where is the workspace?"}"#))
        .expect("submit-turn request should build");
    let stream = crate::helpers::response_text(
        app.clone().oneshot(turn).await.expect("turn route"),
        StatusCode::OK,
        format!("POST /api/v1/sessions/{session_id}/turns"),
    )
    .await;
    let events: Vec<serde_json::Value> = stream
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|data| serde_json::from_str(data).expect("session event data should be JSON"))
        .collect();
    let outcome = events
        .iter()
        .find(|event| {
            event["event"] == "run.session.turn.failed"
                || event["event"] == "run.session.turn.succeeded"
        })
        .unwrap_or_else(|| panic!("the turn ends: {events:?}"));
    let code = outcome["properties"]["code"].as_str().unwrap_or_default();
    assert!(
        !matches!(code, "sandbox_unavailable" | "no_sandbox"),
        "the turn reached the container: {outcome}"
    );
    if outcome["event"] == "run.session.turn.failed" {
        assert_eq!(code, "agent_error", "{outcome}");
        assert!(
            outcome["properties"]["error"]
                .as_str()
                .is_some_and(|error| error.contains("server record")),
            "the turn stopped at the run-tools client, after the sandbox: {outcome}"
        );
    }

    let _ = Command::new("docker")
        .args(["rm", "-f", &container])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(api(path))
        .body(Body::empty())
        .expect("GET request should build")
}

/// The image the server's `docker-small` environment names in the tests
/// below: a runner image with `git` for the checkpoint commit, and not the
/// provider's default, so the container proves the catalog's image reached it.
const CATALOG_IMAGE: &str = "ghcr.io/lithoscomputer/ubuntu-22.04:slim";

/// A Docker environment in the server's catalog, with the image it runs.
async fn create_docker_environment(
    app: &axum::Router,
    id: &str,
    image: &str,
    mode: EnvironmentNetworkMode,
) {
    let environment = serde_json::json!({
        "id": id,
        "provider": "docker",
        "image": { "docker": image, "dockerfile": null },
        "resources": { "cpu": null, "memory": null, "disk": null },
        "network": { "mode": mode, "allow": [] },
        "lifecycle": { "preserve": false, "stop_on_terminal": true, "auto_stop": null },
        "labels": {},
        "env": {}
    });
    let request = Request::builder()
        .method("POST")
        .uri(api("/environments"))
        .header("content-type", "application/json")
        .body(Body::from(environment.to_string()))
        .expect("environment request should build");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("environment request routes");
    response_json(
        response,
        StatusCode::CREATED,
        "POST /api/v1/environments".to_string(),
    )
    .await;
}

/// Create a run from `intent` without starting it: the run's id.
async fn create_run(app: &axum::Router, intent: serde_json::Value) -> String {
    let request = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(intent.to_string()))
        .expect("create request should build");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("create request routes");
    let body = response_json(response, StatusCode::CREATED, "POST /api/v1/runs").await;
    body["id"]
        .as_str()
        .expect("the created run's id")
        .to_string()
}

async fn start_run(app: &axum::Router, run_id: &str) {
    let request = Request::builder()
        .method("POST")
        .uri(api(&format!("/runs/{run_id}/start")))
        .body(Body::empty())
        .expect("start request should build");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("start request routes");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "POST /runs/{run_id}/start"
    );
}

/// The root graph Petri admitted for the run, as the run's blob store holds
/// it: its `params` carry `fabro.environment` and `fabro.launch`, its nodes
/// their step configuration.
async fn admitted_root_graph(app: &axum::Router, run_id: &str) -> serde_json::Value {
    let admission = run_admission(app, run_id).await;
    let blob = admission["graph"]["blob"]
        .as_str()
        .expect("the root graph's blob hash");
    let request = Request::builder()
        .method("GET")
        .uri(api(&format!("/runs/{run_id}/blobs/{blob}")))
        .body(Body::empty())
        .expect("blob request should build");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("blob request routes");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "GET /runs/{run_id}/blobs/{blob}"
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("the graph blob reads");
    serde_json::from_slice(&bytes).expect("the graph blob is JSON")
}

/// A bundle that names a server environment it does not declare admits: the
/// catalog's `[environments.docker-small]` reaches Petri through the
/// settings layer, its image lands on the lowered environment, and, with
/// a Docker daemon, the run's container runs that image.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bundle_naming_a_catalog_environment_runs_on_docker_with_its_image() {
    assert_catalog_environment_runs(EnvironmentNetworkMode::AllowAll, "bridge").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker; creates and deletes a container"]
async fn a_catalog_network_block_reaches_the_created_docker_container() {
    // The shared body returns early without Docker; an explicit run must not.
    assert!(fabro_test::docker_available());
    assert_catalog_environment_runs(EnvironmentNetworkMode::Block, "none").await;
}

async fn assert_catalog_environment_runs(mode: EnvironmentNetworkMode, network_mode: &str) {
    let settings = settings_from_toml("_version = 1\n\n[run.environment]\nid = \"local\"\n");
    let state = test_app_state_with_options(settings, 5);
    let app = test_app_with_scheduler(Arc::clone(&state));
    create_docker_environment(&app, "docker-small", CATALOG_IMAGE, mode).await;

    let version_id = register_version(&app, &[
        ("workflow.fabro", COMMAND_DOT),
        (
            "workflow.toml",
            "_version = 1\n\n[run.environment]\nid = \"docker-small\"\n",
        ),
    ])
    .await;
    let intent = serde_json::json!({
        "workflow_version_id": version_id,
        "target": {"kind": "none"},
        "environment_id": "docker-small",
        "args": {},
    });
    let run_id = create_run(&app, intent).await;
    let graph = admitted_root_graph(&app, &run_id).await;
    let environment = &graph["params"]["fabro.environment"];
    assert_eq!(environment["id"], "docker-small", "{environment}");
    assert_eq!(environment["provider"], "docker", "{environment}");
    assert_eq!(environment["image"], CATALOG_IMAGE, "{environment}");
    assert_eq!(
        graph["params"]["fabro.launch"]["sandbox_backend"], "docker",
        "{}",
        graph["params"]["fabro.launch"]
    );

    if !fabro_test::docker_available() {
        return;
    }
    start_run(&app, &run_id).await;
    let status = helpers::wait_for_run_status_with_timeout(
        &app,
        &run_id,
        &["succeeded", "failed"],
        DOCKER_RUN_TIMEOUT,
    )
    .await;
    let projection = settled_state(&state, &app, &run_id).await;
    assert_eq!(status, "succeeded", "run: {projection}");
    let instance = &projection["sandbox"]["instance"];
    assert_eq!(instance["provider"], "docker", "{instance}");
    assert_eq!(
        instance["image"], CATALOG_IMAGE,
        "the container runs the catalog's image: {instance}"
    );
    let container_id = instance["runtime"]["id"].as_str().expect("container id");
    let inspection = Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{.HostConfig.NetworkMode}}",
            container_id,
        ])
        .output()
        .expect("Docker inspect runs");
    let output = Command::new("docker")
        .args([
            "ps",
            "-aq",
            "--filter",
            &format!("label=petri.run={run_id}"),
        ])
        .output()
        .expect("docker ps runs");
    for container in String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let _ = Command::new("docker")
            .args(["rm", "-f", container])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    assert!(inspection.status.success(), "{inspection:?}");
    assert_eq!(
        String::from_utf8(inspection.stdout)
            .expect("Docker inspect prints UTF-8")
            .trim(),
        network_mode
    );
}

/// A bundle's own `[environments.<id>]` table wins over the server's, key
/// by key: its image replaces the catalog's on the lowered environment.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bundles_own_environment_table_overrides_the_servers() {
    let settings = settings_from_toml("_version = 1\n\n[run.environment]\nid = \"local\"\n");
    let state = test_app_state_with_options(settings, 5);
    let app = test_app_with_scheduler(Arc::clone(&state));
    create_docker_environment(
        &app,
        "docker-small",
        CATALOG_IMAGE,
        EnvironmentNetworkMode::AllowAll,
    )
    .await;

    let version_id = register_version(&app, &[
        ("workflow.fabro", COMMAND_DOT),
        (
            "workflow.toml",
            "_version = 1\n\n[run.environment]\nid = \"docker-small\"\n\n\
             [environments.docker-small]\nprovider = \"docker\"\n\n\
             [environments.docker-small.image]\ndocker = \"alpine:3.19\"\n",
        ),
    ])
    .await;
    let intent = serde_json::json!({
        "workflow_version_id": version_id,
        "target": {"kind": "none"},
        "environment_id": "docker-small",
        "args": {},
    });
    let run_id = create_run(&app, intent).await;
    let graph = admitted_root_graph(&app, &run_id).await;
    let environment = &graph["params"]["fabro.environment"];
    assert_eq!(environment["provider"], "docker", "{environment}");
    assert_eq!(
        environment["image"], "alpine:3.19",
        "the bundle's image over the catalog's: {environment}"
    );
}

/// An environment no layer declares is refused before Petri sees the
/// bundle: the server's own settings resolution refuses it at validation,
/// and an intent naming an environment the catalog lacks is refused at
/// create. Petri's own diagnostic for the same bundle is
/// `fabro-petri::check::an_unknown_environment_is_refused_and_the_launch_selects_over_the_bundle`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_environment_id_is_refused_before_petri() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let state = test_app_state_with_options(test_settings(), 5);
    let app = test_app_with_scheduler(Arc::clone(&state));

    let mut manifest = minimal_manifest_json(COMMAND_DOT);
    manifest["workflows"]["workflow.fabro"]["config"] = serde_json::json!({
        "path": "workflow.toml",
        "source": "_version = 1\n\n[run.environment]\nid = \"nowhere\"\n",
    });
    let request = Request::builder()
        .method("POST")
        .uri(api("/validate"))
        .header("content-type", "application/json")
        .body(Body::from(manifest.to_string()))
        .expect("validate request should build");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("validate request routes");
    let body = response_json(
        response,
        StatusCode::BAD_REQUEST,
        "POST /api/v1/validate".to_string(),
    )
    .await;
    assert_eq!(
        body["errors"][0]["detail"], "failed to resolve manifest settings",
        "{body}"
    );

    let version_id = register_version(&app, &[
        ("workflow.fabro", COMMAND_DOT),
        ("workflow.toml", PLAIN_SETTINGS),
    ])
    .await;
    let mut intent = intent(&version_id, workspace.path());
    intent["environment_id"] = serde_json::json!("nowhere");
    let request = Request::builder()
        .method("POST")
        .uri(api("/runs"))
        .header("content-type", "application/json")
        .body(Body::from(intent.to_string()))
        .expect("create request should build");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("create request routes");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("the response body reads");
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        status.is_client_error() && text.contains("nowhere"),
        "the server refuses an environment its catalog lacks: {status} {text}"
    );
}

/// An agent workflow with one stage.
const AGENT_DOT: &str = r#"digraph Agent {
    graph [goal="Greet with the notes server available"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    greet [prompt="Say hello. Use no tools."]
    start -> greet -> exit
}"#;

/// A bundle referencing a catalog MCP server by id admits without declaring
/// it: the server's catalog reaches Petri, the entry lands on the agent
/// node under the reference's name, and the agent session lists the
/// server's tool to the model.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bundle_naming_a_catalog_mcp_server_lists_its_tools_to_the_model() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let twin = twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    TwinScenarios::new(&namespace)
        .scenario(TwinScenario::responses(OPENAI_MODEL).text("Hello."))
        .load(twin)
        .await;
    let settings = test_settings();
    let state = TestAppStateBuilder::new()
        .runtime_settings(settings.server_settings, settings.manifest_run_defaults)
        .max_concurrent_runs(5)
        .in_process_execution()
        .llm_overlay(llm_overlay_with_provider_base_url(
            "openai",
            twin.base_url.clone(),
        ))
        .vault_entries([(EnvVars::OPENAI_API_KEY, namespace.clone())])
        .build();
    let app = test_app_with_scheduler(Arc::clone(&state));

    // The catalog entry: the echo server over stdio, checked in under `test/`.
    let server = crate::helpers::repo_root().join("test/mcp/echo_server.py");
    let definition = serde_json::json!({
        "id": "echo-prod",
        "display_name": "Echo",
        "description": "The scenario tests' echo server.",
        "transport": {
            "type": "stdio",
            "command": ["python3", server.to_string_lossy()],
            "env": {}
        },
        "startup_timeout_secs": 10,
        "tool_timeout_secs": 60
    });
    let request = Request::builder()
        .method("POST")
        .uri(api("/mcp-servers"))
        .header("content-type", "application/json")
        .body(Body::from(definition.to_string()))
        .expect("mcp server request should build");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("mcp server request routes");
    response_json(
        response,
        StatusCode::CREATED,
        "POST /api/v1/mcp-servers".to_string(),
    )
    .await;

    let version_id = register_version(&app, &[
        ("workflow.fabro", AGENT_DOT),
        (
            "workflow.toml",
            "_version = 1\n\n[run.agent.mcps.notes]\nid = \"echo-prod\"\n",
        ),
    ])
    .await;
    let mut intent = intent(&version_id, workspace.path());
    intent["args"]["model"] = serde_json::json!(OPENAI_MODEL);
    let run_id = create_and_start_run_from_intent(&app, intent).await;

    let graph = admitted_root_graph(&app, &run_id).await;
    let greet = graph["nodes"]
        .as_array()
        .expect("the graph's nodes")
        .iter()
        .find(|node| node["name"] == "greet")
        .unwrap_or_else(|| panic!("the greet node: {graph}"));
    let mcps = &greet["step"]["config"]["mcps"];
    assert_eq!(mcps.as_array().map(Vec::len), Some(1), "{greet}");
    assert_eq!(mcps[0]["name"], "notes", "the reference's name: {mcps}");
    assert_eq!(mcps[0]["source"], "mcp-catalog:echo-prod", "{mcps}");
    assert_eq!(mcps[0]["transport"]["type"], "stdio", "{mcps}");

    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(
        status,
        "succeeded",
        "run: {}",
        run_json(&app, &run_id).await
    );
    // The tools the session offered the model, as Petri records them once
    // per session (`attractor.tools`) and the projection lists them.
    let projection = settled_state(&state, &app, &run_id).await;
    let tools: Vec<&str> = projection["stages"]["greet@1"]["agent_tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        tools.contains(&"mcp__notes__echo"),
        "the session lists the catalog server's tool under the reference's name: {tools:?}"
    );
}

/// The hello bundle's agent stage, run with the given version files and an
/// optional intent goal override, to completion: the run's id, the twin's
/// request-log namespace and the server state.
async fn run_hello_agent(
    files: &[(&str, &str)],
    goal: Option<&str>,
) -> (Arc<AppState>, axum::Router, String, String) {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let twin = twin_openai().await;
    let namespace = format!(
        "{}::{}::{}",
        module_path!(),
        line!(),
        goal.map_or("file", |_| "override")
    );
    TwinScenarios::new(&namespace)
        .scenario(TwinScenario::responses(OPENAI_MODEL).text("A limerick, added."))
        .scenario(TwinScenario::responses(OPENAI_MODEL).text("A limerick, added."))
        .load(twin)
        .await;
    let settings = test_settings();
    let state = TestAppStateBuilder::new()
        .runtime_settings(settings.server_settings, settings.manifest_run_defaults)
        .max_concurrent_runs(5)
        .in_process_execution()
        .llm_overlay(llm_overlay_with_provider_base_url(
            "openai",
            twin.base_url.clone(),
        ))
        .vault_entries([(EnvVars::OPENAI_API_KEY, namespace.clone())])
        .build();
    let app = test_app_with_scheduler(Arc::clone(&state));
    let version_id = register_version(&app, files).await;
    let mut intent = intent(&version_id, workspace.path());
    intent["args"]["model"] = serde_json::json!(OPENAI_MODEL);
    if let Some(goal) = goal {
        intent["goal"] = serde_json::json!(goal);
    }
    let run_id = create_and_start_run_from_intent(&app, intent).await;
    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    let run = run_json(&app, &run_id).await;
    assert_eq!(status, "succeeded", "run: {run}");
    // The workspace outlives the run: the run's record names it.
    std::mem::forget(workspace);
    (state, app, namespace, run_id)
}

/// The goal the run shows is the goal its stages execute with: `GET
/// /runs/{id}` names it, Petri's admitted graph carries it, and the agent
/// stage's prompt to the model opens with it in place of the graph's own.
async fn assert_run_goal(app: &axum::Router, namespace: &str, run_id: &str, goal: &str) {
    let run = run_json(app, run_id).await;
    assert_eq!(run["goal"], goal, "the run shows the goal: {run}");
    let graph = admitted_root_graph(app, run_id).await;
    assert_eq!(
        graph["params"]["goal"], goal,
        "Petri admitted the run's goal: {}",
        graph["params"]
    );
    let logs = twin_openai().await.request_logs(namespace).await;
    let prompt = logs["requests"]
        .as_array()
        .expect("twin request logs are an array")
        .iter()
        .filter_map(|request| request["input_text"].as_str())
        .find(|input| input.contains("Add a haiku to the README"))
        .unwrap_or_else(|| panic!("the agent stage's prompt reached the twin, got {logs}"));
    assert!(
        prompt.contains(goal),
        "the agent's prompt carries the run's goal, got {prompt}"
    );
    assert!(
        !prompt.contains("Say hello and demonstrate a basic Fabro workflow"),
        "the graph's own goal is replaced, got {prompt}"
    );
}

/// An intent's goal override is bound into Petri's check, so the agent
/// stages execute with the goal the run shows, not the workflow's own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_goal_override_is_the_goal_the_stages_execute_with() {
    const GOAL: &str = "Add a limerick to the README instead of a haiku";
    let [(workflow_path, workflow), (settings_path, settings)] = hello_files();
    let (_state, app, namespace, run_id) = run_hello_agent(
        &[(workflow_path, &workflow), (settings_path, &settings)],
        Some(GOAL),
    )
    .await;
    assert_run_goal(&app, &namespace, &run_id, GOAL).await;
}

/// A `[run.goal] file` layer in the bundle's `workflow.toml` is the run's
/// goal the same way: the file's text is what the run shows and what the
/// agent stage executes with.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_goal_file_layer_is_the_goal_the_stages_execute_with() {
    const GOAL: &str = "Write a limerick about workflow engines into the README";
    let [(workflow_path, workflow), _] = hello_files();
    let settings =
        "_version = 1\n[workflow]\ngraph = \"workflow.fabro\"\n[run.goal]\nfile = \"goal.md\"\n";
    let (_state, app, namespace, run_id) = run_hello_agent(
        &[
            (workflow_path, &workflow),
            ("workflow.toml", settings),
            ("goal.md", GOAL),
        ],
        None,
    )
    .await;
    assert_run_goal(&app, &namespace, &run_id, GOAL).await;
}
