#![allow(
    clippy::absolute_paths,
    clippy::manual_assert,
    clippy::redundant_closure_for_method_calls,
    reason = "These CLI harness helpers value explicit fixtures over pedantic style lints."
)]
#![expect(
    clippy::disallowed_methods,
    reason = "These CLI integration test helpers shell out to real git and fabro binaries while constructing fixtures."
)]

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fabro_config::bind::Bind;
use fabro_config::daemon::ServerDaemon;
use fabro_config::{Storage, envfile};
use fabro_store::EventEnvelope;
use fabro_test::{TestContext, expect_reqwest_status};
use fabro_types::RunId;
use httpmock::{Mock, MockServer};
use serde_json::Value;
use shlex::try_quote;

use crate::support::unique_run_id;

const LOCAL_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const CI_COMMAND_TIMEOUT: Duration = Duration::from_secs(90);
static NEXT_SEEDED_EVENT_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) use fabro_store::RunProjection;

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct RunSummaryRecord {
    run_id: String,
    #[serde(default)]
    labels: std::collections::HashMap<String, String>,
}

pub(crate) struct RunSetup {
    pub(crate) run_id:  String,
    pub(crate) run_dir: PathBuf,
}

pub(crate) struct SeededGitRunSetup {
    pub(crate) run:          RunSetup,
    pub(crate) step_one_sha: String,
}

pub(crate) struct ProjectFixture {
    pub(crate) project_dir: PathBuf,
    pub(crate) fabro_root:  PathBuf,
}

pub(crate) struct WorkspaceRunSetup {
    pub(crate) run:           RunSetup,
    pub(crate) workspace_dir: PathBuf,
}

pub(crate) struct WorkflowGate {
    gate_path: PathBuf,
}

#[derive(Clone, Copy)]
enum SeededRunState {
    Submitted,
    Completed,
}

fn command_timeout() -> Duration {
    if std::env::var_os("CI").is_some() {
        CI_COMMAND_TIMEOUT
    } else {
        LOCAL_COMMAND_TIMEOUT
    }
}

/// Returns the repo-relative path to a test fixture.
///
/// Prefer `TestContext::install_fixture` for tests that run `fabro run`,
/// since config discovery walks from the workflow file's parent directory
/// and can find the repo's `.fabro/project.toml`.
pub(crate) fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../../test/{name}"))
        .canonicalize()
        .expect("fixture path should exist")
}

pub(crate) fn output_stderr(output: &Output) -> String {
    stderr(output)
}

pub(crate) fn output_stdout(output: &Output) -> String {
    stdout(output)
}

pub(crate) fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

pub(crate) fn mock_resolved_run<'a>(
    server: &'a MockServer,
    selector: &str,
    run_id: &str,
) -> Mock<'a> {
    server.mock(|when, then| {
        when.method("GET")
            .path("/api/v1/runs/resolve")
            .query_param("selector", selector);
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "run_id": run_id,
                "workflow_name": "Nightly Build",
                "workflow_slug": "nightly-build",
                "goal": "Nightly run",
                "title": "Nightly run",
                "labels": {},
                "source_directory": null,
                "repository": { "name": "unknown" },
                "start_time": "2026-04-05T12:00:00Z",
                "created_at": "2026-04-05T12:00:00Z",
                "status": {
                    "kind": "succeeded",
                    "reason": "completed"
                },
                "pending_control": null,
                "duration_ms": 123,
                "elapsed_secs": 0,
                "total_usd_micros": null
            }));
    })
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout should be valid UTF-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr should be valid UTF-8")
}

pub(crate) fn run_success(context: &TestContext, args: &[&str]) -> Output {
    run_success_in(context, args, &context.temp_dir)
}

fn run_success_in(context: &TestContext, args: &[&str], cwd: &Path) -> Output {
    let mut cmd = context.command();
    cmd.current_dir(cwd);
    cmd.timeout(command_timeout());
    cmd.args(args);
    let output = cmd.output().expect("command should execute");
    if !output.status.success() {
        panic!(
            "command failed: fabro {}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            stdout(&output),
            stderr(&output)
        );
    }
    output
}

pub(crate) fn setup_completed_dry_run(context: &TestContext) -> RunSetup {
    let workflow = context.install_fixture("simple.fabro");
    run_completed_dry_run(context, &workflow)
}

pub(crate) fn setup_completed_fast_dry_run(context: &TestContext) -> RunSetup {
    let workflow = fast_simple_workflow(context);
    run_completed_dry_run(context, &workflow)
}

pub(crate) fn setup_seeded_completed_dry_run(context: &TestContext) -> RunSetup {
    block_on(seed_dry_run(context, SeededRunState::Completed))
}

pub(crate) fn setup_seeded_created_dry_run(context: &TestContext) -> RunSetup {
    block_on(seed_dry_run(context, SeededRunState::Submitted))
}

fn run_completed_dry_run(context: &TestContext, workflow: &Path) -> RunSetup {
    let run_id = unique_run_id();
    let mut cmd = context.run_cmd();
    cmd.current_dir(&context.temp_dir);
    cmd.timeout(command_timeout());
    cmd.args([
        "--run-id",
        run_id.as_str(),
        "--dry-run",
        "--auto-approve",
        "--no-retro",
        "--sandbox",
        "local",
    ]);
    cmd.arg(workflow);
    let output = cmd.output().expect("command should execute");
    if !output.status.success() {
        panic!(
            "command failed: fabro run --dry-run --auto-approve --no-retro --sandbox local {}\nstdout:\n{}\nstderr:\n{}",
            workflow.display(),
            stdout(&output),
            stderr(&output)
        );
    }
    let run_setup = RunSetup {
        run_dir: context.find_run_dir(&run_id),
        run_id,
    };
    wait_for_event_names(&run_setup.run_dir, &[
        "run.completed",
        "sandbox.cleanup.completed",
    ]);
    run_setup
}

fn fast_simple_workflow(context: &TestContext) -> PathBuf {
    let workflow = context.temp_dir.join("simple.fabro");
    if !workflow.exists() {
        write_text_file(
            &workflow,
            r#"digraph Simple {
    graph [goal="Run tests and report results"]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    run_tests [shape=parallelogram, label="Run Tests", script="true"]
    report    [shape=parallelogram, label="Report", script="true"]

    start -> run_tests -> report -> exit
}
"#,
        );
    }
    workflow
}

#[expect(
    clippy::disallowed_methods,
    reason = "This sync integration helper polls run artifacts after spawning a detached CLI process."
)]
pub(crate) fn setup_detached_dry_run(context: &TestContext) -> RunSetup {
    let workflow = context.install_fixture("simple.fabro");
    let run_id = unique_run_id();
    let mut cmd = context.run_cmd();
    cmd.current_dir(&context.temp_dir);
    cmd.timeout(command_timeout());
    cmd.args([
        "--run-id",
        run_id.as_str(),
        "--detach",
        "--dry-run",
        "--auto-approve",
        "--no-retro",
        "--sandbox",
        "local",
    ]);
    cmd.arg(workflow);
    let output = cmd.output().expect("command should execute");
    if !output.status.success() {
        panic!(
            "command failed: fabro run --detach --dry-run --auto-approve --no-retro --sandbox local {}\nstdout:\n{}\nstderr:\n{}",
            fixture("simple.fabro").display(),
            stdout(&output),
            stderr(&output)
        );
    }
    assert_eq!(stdout(&output).trim(), run_id);
    let run = resolve_run(context, &run_id);
    let deadline = Instant::now() + command_timeout();
    while run_events(&run.run_dir).is_empty() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for store events for {run_id}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    run
}

pub(crate) fn setup_seeded_git_backed_changed_run(context: &TestContext) -> SeededGitRunSetup {
    block_on(seed_git_backed_changed_run(context))
}

pub(crate) fn setup_seeded_git_backed_noop_run(context: &TestContext) -> RunSetup {
    block_on(seed_git_backed_noop_run(context))
}

pub(crate) fn setup_seeded_artifact_run(context: &TestContext) -> RunSetup {
    block_on(seed_artifact_run(context))
}

pub(crate) fn setup_project_fixture(context: &TestContext) -> ProjectFixture {
    let project_dir = context.temp_dir.join("project");
    let fabro_root = project_dir.join(".fabro");
    write_text_file(&project_dir.join(".fabro/project.toml"), "_version = 1\n");
    std::fs::create_dir_all(fabro_root.join("workflows"))
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", fabro_root.display()));
    ProjectFixture {
        project_dir,
        fabro_root,
    }
}

impl WorkflowGate {
    pub(crate) fn release(&self) {
        write_text_file(&self.gate_path, "open\n");
    }
}

pub(crate) fn setup_local_sandbox_run(context: &TestContext) -> WorkspaceRunSetup {
    let workspace_dir = context.temp_dir.join("local-sandbox");
    std::fs::create_dir_all(&workspace_dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", workspace_dir.display()));

    write_text_file(
        &workspace_dir.join("sandbox_run.fabro"),
        r#"digraph SandboxRun {
  graph [goal="Exercise sandbox commands", default_max_retries=0]
  start [shape=Mdiamond]
  exit [shape=Msquare]
  populate_sandbox [shape=parallelogram, script="mkdir -p sandbox_dir/download_me/nested && printf keep > sandbox_dir/download_me/root.txt && printf nested > sandbox_dir/download_me/nested/child.txt", max_retries=0]
  start -> populate_sandbox -> exit
}
"#,
    );
    write_text_file(
        &workspace_dir.join("run.toml"),
        r#"_version = 1

[workflow]
graph = "sandbox_run.fabro"

[run]
goal = "Exercise sandbox commands"

[run.sandbox]
provider = "local"
preserve = true

[run.sandbox.local]
worktree_mode = "never"
"#,
    );

    let run = run_local_workflow(context, &workspace_dir, "run.toml");
    assert!(run_state(&run.run_dir).sandbox.is_some());

    WorkspaceRunSetup { run, workspace_dir }
}

fn run_local_workflow(context: &TestContext, workspace_dir: &Path, workflow: &str) -> RunSetup {
    let run_id = unique_run_id();
    let mut cmd = context.run_cmd();
    cmd.current_dir(workspace_dir);
    cmd.timeout(command_timeout());
    cmd.env("OPENAI_API_KEY", "test");
    cmd.args([
        "--run-id",
        run_id.as_str(),
        "--auto-approve",
        "--no-retro",
        "--sandbox",
        "local",
        "--provider",
        "openai",
        workflow,
    ]);
    let output = cmd.output().expect("command should execute");
    if !output.status.success() {
        panic!(
            "command failed: fabro run --auto-approve --no-retro --sandbox local --provider openai {workflow}\nstdout:\n{}\nstderr:\n{}",
            stdout(&output),
            stderr(&output)
        );
    }

    RunSetup {
        run_dir: context.find_run_dir(&run_id),
        run_id,
    }
}

pub(crate) fn add_project_workflow(
    project: &ProjectFixture,
    name: &str,
    goal: &str,
    dot_source: &str,
) -> PathBuf {
    let workflow_dir = project.fabro_root.join("workflows").join(name);
    std::fs::create_dir_all(&workflow_dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", workflow_dir.display()));
    write_text_file(&workflow_dir.join("workflow.fabro"), dot_source);
    write_text_file(
        &workflow_dir.join("workflow.toml"),
        &format!(
            "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n\n[run]\ngoal = {goal:?}\n"
        ),
    );
    workflow_dir
}

pub(crate) fn add_user_workflow(context: &TestContext, name: &str, goal: &str) -> PathBuf {
    let workflow_dir = context.home_dir.join(".fabro/workflows").join(name);
    std::fs::create_dir_all(&workflow_dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", workflow_dir.display()));
    write_text_file(
        &workflow_dir.join("workflow.toml"),
        &format!(
            "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n\n[run]\ngoal = {goal:?}\n"
        ),
    );
    write_text_file(
        &workflow_dir.join("workflow.fabro"),
        &format!(
            "digraph {} {{\n  graph [goal={goal:?}]\n  start [shape=Mdiamond]\n  exit [shape=Msquare]\n  start -> exit\n}}\n",
            to_pascal_case(name),
        ),
    );
    workflow_dir
}

pub(crate) fn write_gated_workflow(path: &Path, name: &str, goal: &str) -> WorkflowGate {
    let gate_path = path.with_extension("gate");
    let _ = std::fs::remove_file(&gate_path);
    let gate_path_str = gate_path.to_string_lossy().into_owned();
    let quoted_gate_path = try_quote(&gate_path_str)
        .unwrap_or_else(|_| panic!("failed to quote {}", gate_path.display()));
    write_text_file(
        path,
        &format!(
            "digraph {} {{\n  graph [goal={goal:?}]\n  start [shape=Mdiamond]\n  exit [shape=Msquare]\n  wait [shape=parallelogram, script=\"while [ ! -f {quoted_gate_path} ]; do sleep 0.01; done\"]\n  start -> wait -> exit\n}}\n",
            to_pascal_case(name),
        ),
    );
    WorkflowGate { gate_path }
}

#[expect(
    clippy::disallowed_methods,
    reason = "This sync integration helper polls stored run status without requiring a Tokio runtime."
)]
pub(crate) fn wait_for_status(run_dir: &Path, expected: &[&str]) -> String {
    let deadline = Instant::now() + command_timeout();
    loop {
        if let Some(status) = run_state(run_dir).status.map(|status| match status {
            fabro_types::RunStatus::Submitted => "submitted",
            fabro_types::RunStatus::Queued => "queued",
            fabro_types::RunStatus::Starting => "starting",
            fabro_types::RunStatus::Running => "running",
            fabro_types::RunStatus::Blocked { .. } => "blocked",
            fabro_types::RunStatus::Paused { .. } => "paused",
            fabro_types::RunStatus::Removing => "removing",
            fabro_types::RunStatus::Succeeded { .. } => "succeeded",
            fabro_types::RunStatus::Failed { .. } => "failed",
            fabro_types::RunStatus::Dead => "dead",
            fabro_types::RunStatus::Archived { .. } => "archived",
        }) {
            if expected.contains(&status) {
                return status.to_string();
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for status {:?} in {}",
            expected,
            run_dir.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) fn run_count_for_test_case(context: &TestContext) -> usize {
    run_dirs_for_test_case(context).len()
}

fn run_dirs_for_test_case(context: &TestContext) -> Vec<PathBuf> {
    let runs: Option<Vec<RunSummaryRecord>> = block_on(try_get_server_json_for_storage(
        &context.storage_dir,
        "/api/v1/runs",
    ));
    let Some(runs) = runs else {
        return Vec::new();
    };
    runs.into_iter()
        .filter(|run| {
            run.labels
                .get("fabro_test_case")
                .is_some_and(|value| value == context.test_case_id())
        })
        .filter_map(|run| find_run_dir(&context.storage_dir, &run.run_id))
        .collect()
}

pub(crate) fn git_filters(context: &TestContext) -> Vec<(String, String)> {
    let mut filters = context.filters();
    filters.push((r"\b[0-9a-f]{7,40}\b".to_string(), "[SHA]".to_string()));
    filters.push((
        r"(fabro resume )[0-9A-HJKMNP-TV-Z]{8}\b".to_string(),
        "$1[RUN_PREFIX]".to_string(),
    ));
    filters.push((
        r"(Forked run )[0-9A-HJKMNP-TV-Z]{8}\b".to_string(),
        "$1[RUN_PREFIX]".to_string(),
    ));
    filters.push((
        r"(-> )[0-9A-HJKMNP-TV-Z]{8}\b".to_string(),
        "$1[RUN_PREFIX]".to_string(),
    ));
    filters.push((
        r"(Rewound )[0-9A-HJKMNP-TV-Z]{8}\b".to_string(),
        "$1[RUN_PREFIX]".to_string(),
    ));
    filters.push((
        r"(; new run )[0-9A-HJKMNP-TV-Z]{8}\b".to_string(),
        "$1[RUN_PREFIX]".to_string(),
    ));
    filters
}

#[expect(
    clippy::disallowed_methods,
    reason = "This sync integration helper polls for the run directory to appear without requiring a Tokio runtime."
)]
pub(crate) fn resolve_run(context: &TestContext, run_id: &str) -> RunSetup {
    let deadline = Instant::now() + command_timeout();
    loop {
        if let Some(run_dir) = find_run_dir(&context.storage_dir, run_id) {
            return RunSetup {
                run_id: run_id.to_string(),
                run_dir,
            };
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for run dir for {run_id}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) fn find_run_dir(storage_dir: &Path, run_id: &str) -> Option<PathBuf> {
    if let Ok(run_id) = run_id.parse::<RunId>() {
        let run_dir = Storage::new(storage_dir)
            .run_scratch(&run_id)
            .root()
            .to_path_buf();
        if run_dir.is_dir() {
            return Some(run_dir);
        }
    }

    let runs_dir = storage_dir.join("scratch");
    let entries = std::fs::read_dir(&runs_dir).ok()?;
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(run_id))
        })
}

fn infer_run_id(run_dir: &Path) -> String {
    run_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .and_then(|name| name.rsplit('-').next().map(ToOwned::to_owned))
        .filter(|value| !value.is_empty())
        .expect("run directory name should contain run id suffix")
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime should build")
        .block_on(future)
}

pub(crate) fn local_dev_token(storage_dir: &Path) -> Option<String> {
    let server_state = Storage::new(storage_dir).runtime_directory();

    envfile::read_env_file(&server_state.env_path())
        .ok()
        .and_then(|entries| entries.get("FABRO_DEV_TOKEN").cloned())
        .or_else(|| fabro_util::dev_token::read_dev_token_file(&server_state.dev_token_path()))
}

pub(crate) fn server_endpoint(storage_dir: &Path) -> Option<(fabro_http::HttpClient, String)> {
    let runtime_directory = Storage::new(storage_dir).runtime_directory();
    let daemon = ServerDaemon::read(&runtime_directory).ok().flatten()?;
    let mut headers = fabro_http::HeaderMap::new();
    if let Some(token) = local_dev_token(storage_dir) {
        headers.insert(
            fabro_http::header::AUTHORIZATION,
            fabro_http::HeaderValue::from_str(&format!("Bearer {token}"))
                .expect("local dev token should build an authorization header"),
        );
    }
    match daemon.bind {
        Bind::Unix(path) if path.exists() => Some((
            fabro_http::HttpClientBuilder::new()
                .unix_socket(path)
                .no_proxy()
                .default_headers(headers.clone())
                .build()
                .expect("test Unix-socket HTTP client should build"),
            "http://fabro".to_string(),
        )),
        Bind::Unix(_) => None,
        Bind::Tcp(addr) => Some((
            fabro_http::HttpClientBuilder::new()
                .no_proxy()
                .default_headers(headers)
                .build()
                .expect("test TCP HTTP client should build"),
            format!("http://{addr}"),
        )),
    }
}

pub(crate) fn server_target(storage_dir: &Path) -> String {
    let runtime_directory = Storage::new(storage_dir).runtime_directory();
    let daemon = ServerDaemon::read(&runtime_directory)
        .expect("server record should parse")
        .expect("server record should exist");
    daemon.bind.to_target()
}

async fn get_server_json<T: serde::de::DeserializeOwned>(run_dir: &Path, path: &str) -> T {
    let runs_dir = run_dir.parent().expect("run dir should have parent");
    let storage_dir = runs_dir.parent().expect("runs dir should have parent");
    get_server_json_for_storage(storage_dir, path).await
}

async fn try_get_server_json_for_storage<T: serde::de::DeserializeOwned>(
    storage_dir: &Path,
    path: &str,
) -> Option<T> {
    let (client, base_url) = server_endpoint(storage_dir)?;
    let response = client.get(format!("{base_url}{path}")).send().await.ok()?;
    let status = response.status();
    if status != fabro_http::StatusCode::OK {
        return None;
    }
    response.json::<T>().await.ok()
}

async fn get_server_json_for_storage<T: serde::de::DeserializeOwned>(
    storage_dir: &Path,
    path: &str,
) -> T {
    let (client, base_url) = server_endpoint(storage_dir).expect("server endpoint should exist");
    let response = client
        .get(format!("{base_url}{path}"))
        .send()
        .await
        .expect("server request should succeed");
    let response =
        expect_reqwest_status(response, fabro_http::StatusCode::OK, format!("GET {path}")).await;
    response
        .json::<T>()
        .await
        .expect("server response should parse")
}

pub(crate) fn run_state(run_dir: &Path) -> RunProjection {
    let run_id = infer_run_id(run_dir);
    block_on(get_server_json(
        run_dir,
        &format!("/api/v1/runs/{run_id}/state"),
    ))
}

pub(crate) fn run_state_by_id(context: &TestContext, run_id: &str) -> RunProjection {
    block_on(get_server_json_for_storage(
        &context.storage_dir,
        &format!("/api/v1/runs/{run_id}/state"),
    ))
}

pub(crate) fn run_events(run_dir: &Path) -> Vec<EventEnvelope> {
    let run_id = infer_run_id(run_dir);
    let response: serde_json::Value = block_on(get_server_json(
        run_dir,
        &format!("/api/v1/runs/{run_id}/events"),
    ));
    crate::support::parse_event_envelopes(&response)
}

#[expect(
    clippy::disallowed_methods,
    reason = "This sync integration helper polls stored events without requiring a Tokio runtime."
)]
pub(crate) fn wait_for_event_names(run_dir: &Path, expected: &[&str]) {
    let deadline = std::time::Instant::now() + command_timeout();

    loop {
        let event_names = run_events(run_dir)
            .into_iter()
            .map(|event| event.event.event_name().to_string())
            .collect::<Vec<_>>();

        if expected
            .iter()
            .all(|expected_name| event_names.iter().any(|name| name == expected_name))
        {
            return;
        }

        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for events {expected:?}; saw {event_names:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

async fn seed_dry_run(context: &TestContext, state: SeededRunState) -> RunSetup {
    let run = create_seeded_run(
        context,
        "simple.fabro",
        fast_simple_workflow_source(),
        serde_json::json!({
            "dry_run": true,
            "auto_approve": true,
            "no_retro": true,
            "sandbox": "local",
            "label": test_labels(context),
        }),
        None,
    )
    .await;

    if matches!(state, SeededRunState::Completed) {
        let (client, base_url) = server_endpoint(&context.storage_dir)
            .expect("test server endpoint should be available for seeded run events");
        append_seeded_simple_completion_events(&client, &base_url, &run, context).await;
    }

    run
}

async fn seed_git_backed_changed_run(context: &TestContext) -> SeededGitRunSetup {
    let base_sha = "1111111111111111111111111111111111111111";
    let step_one_sha = "2222222222222222222222222222222222222222";
    let step_two_sha = "3333333333333333333333333333333333333333";
    let run = create_seeded_run(
        context,
        "flow.fabro",
        changed_git_workflow_source(),
        serde_json::json!({
            "provider": "openai",
            "sandbox": "local",
            "no_retro": true,
            "label": test_labels(context),
        }),
        Some(serde_json::json!({
            "origin_url": "https://github.com/fabro-sh/seeded-fixture.git",
            "branch": "main",
            "sha": base_sha,
            "dirty": "clean",
            "push_outcome": {
                "type": "succeeded",
                "remote": "origin",
                "branch": "main",
            },
        })),
    )
    .await;

    let (client, base_url) = server_endpoint(&context.storage_dir)
        .expect("test server endpoint should be available for seeded run events");
    append_seeded_git_completion_events(
        &client,
        &base_url,
        &run,
        context,
        base_sha,
        step_one_sha,
        step_two_sha,
    )
    .await;

    SeededGitRunSetup {
        run,
        step_one_sha: step_one_sha.to_string(),
    }
}

async fn seed_git_backed_noop_run(context: &TestContext) -> RunSetup {
    let base_sha = "1111111111111111111111111111111111111111";
    let run = create_seeded_run(
        context,
        "flow.fabro",
        noop_git_workflow_source(),
        serde_json::json!({
            "provider": "openai",
            "sandbox": "local",
            "no_retro": true,
            "label": test_labels(context),
        }),
        Some(serde_json::json!({
            "origin_url": "https://github.com/fabro-sh/seeded-fixture.git",
            "branch": "main",
            "sha": base_sha,
            "dirty": "clean",
            "push_outcome": {
                "type": "succeeded",
                "remote": "origin",
                "branch": "main",
            },
        })),
    )
    .await;

    let (client, base_url) = server_endpoint(&context.storage_dir)
        .expect("test server endpoint should be available for seeded run events");
    append_seeded_git_noop_events(&client, &base_url, &run, context, base_sha).await;
    run
}

async fn seed_artifact_run(context: &TestContext) -> RunSetup {
    let run = create_seeded_run(
        context,
        "artifact_run.fabro",
        artifact_workflow_source(),
        serde_json::json!({
            "sandbox": "local",
            "no_retro": true,
            "label": test_labels(context),
        }),
        None,
    )
    .await;

    let (client, base_url) = server_endpoint(&context.storage_dir)
        .expect("test server endpoint should be available for seeded artifacts");
    append_seeded_artifact_run_events(&client, &base_url, &run, context).await;
    for (stage_id, path, contents) in [
        ("create_assets@1", "assets/node_a/summary.txt", "alpha"),
        ("create_assets@1", "assets/shared/report.txt", "one"),
        ("create_colliding@1", "assets/other/summary.txt", "beta"),
        ("create_colliding@1", "assets/retry/report.txt", "second"),
        ("retry_assets@1", "assets/retry/report.txt", "first"),
        ("retry_assets@2", "assets/retry/report.txt", "second"),
    ] {
        upload_seeded_artifact(&client, &base_url, &run.run_id, stage_id, path, contents).await;
    }

    run
}

async fn create_seeded_run(
    context: &TestContext,
    target_path: &str,
    source: &str,
    args: serde_json::Value,
    git: Option<serde_json::Value>,
) -> RunSetup {
    let run_id = unique_run_id();
    let mut manifest = serde_json::json!({
        "version": 1,
        "run_id": run_id.as_str(),
        "cwd": context.temp_dir.display().to_string(),
        "target": {
            "identifier": target_path,
            "path": target_path,
        },
        "args": args,
        "workflows": {
            (target_path): {
                "source": source,
                "files": {},
            },
        },
    });
    if let Some(git) = git {
        manifest["git"] = git;
    }

    let (client, base_url) = server_endpoint(&context.storage_dir)
        .expect("test server endpoint should be available for seeded run creation");
    let response = client
        .post(format!("{base_url}/api/v1/runs"))
        .header("user-agent", "fabro-cli/test")
        .json(&manifest)
        .send()
        .await
        .expect("seeded run create request should execute");
    let response = expect_reqwest_status(
        response,
        fabro_http::StatusCode::CREATED,
        "POST /api/v1/runs for seeded fixture",
    )
    .await;
    let body: serde_json::Value = response
        .json()
        .await
        .expect("seeded run create response should parse");
    assert_eq!(
        body["id"].as_str(),
        Some(run_id.as_str()),
        "seeded run should use requested run id"
    );

    RunSetup {
        run_dir: context.find_run_dir(&run_id),
        run_id,
    }
}

async fn append_seeded_simple_completion_events(
    client: &fabro_http::HttpClient,
    base_url: &str,
    run: &RunSetup,
    context: &TestContext,
) {
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "sandbox.ready",
        serde_json::json!({
            "provider": "local",
            "duration_ms": 1,
            "name": null,
            "cpu": null,
            "memory": null,
            "url": null,
        }),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "sandbox.initialized",
        serde_json::json!({
            "working_directory": context.temp_dir.display().to_string(),
            "provider": "local",
            "identifier": null,
            "repo_cloned": false,
            "clone_origin_url": null,
            "clone_branch": null,
        }),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "run.started",
        serde_json::json!({
            "name": "Simple",
            "base_branch": null,
            "base_sha": null,
            "run_branch": null,
            "worktree_dir": null,
            "goal": "Run tests and report results",
        }),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "run.starting",
        serde_json::json!({}),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "run.running",
        serde_json::json!({}),
    )
    .await;

    append_seeded_stage(client, base_url, &run.run_id, "start", "Start", 0, None).await;
    append_seeded_edge(client, base_url, &run.run_id, "start", "run_tests").await;
    append_seeded_stage(
        client,
        base_url,
        &run.run_id,
        "run_tests",
        "Run Tests",
        1,
        Some("Dry run: would execute `true`."),
    )
    .await;
    append_seeded_edge(client, base_url, &run.run_id, "run_tests", "report").await;
    append_seeded_stage(
        client,
        base_url,
        &run.run_id,
        "report",
        "Report",
        2,
        Some("Dry run: would execute `true`."),
    )
    .await;
    append_seeded_edge(client, base_url, &run.run_id, "report", "exit").await;
    append_seeded_stage(client, base_url, &run.run_id, "exit", "Exit", 3, None).await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        Some("report"),
        "checkpoint.completed",
        checkpoint_properties(
            "success",
            "report",
            &["start", "run_tests", "report"],
            Some("exit"),
            None,
            None,
        ),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "run.completed",
        serde_json::json!({
            "duration_ms": 123,
            "artifact_count": 0,
            "status": "success",
            "reason": "completed",
            "total_usd_micros": null,
            "final_git_commit_sha": null,
            "final_patch": null,
            "billing": null,
        }),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "sandbox.cleanup.started",
        serde_json::json!({
            "provider": "local",
        }),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "sandbox.cleanup.completed",
        serde_json::json!({
            "provider": "local",
            "duration_ms": 1,
        }),
    )
    .await;
}

async fn append_seeded_git_completion_events(
    client: &fabro_http::HttpClient,
    base_url: &str,
    run: &RunSetup,
    context: &TestContext,
    base_sha: &str,
    step_one_sha: &str,
    step_two_sha: &str,
) {
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "sandbox.ready",
        serde_json::json!({
            "provider": "local",
            "duration_ms": 1,
            "name": null,
            "cpu": null,
            "memory": null,
            "url": null,
        }),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "sandbox.initialized",
        serde_json::json!({
            "working_directory": context.temp_dir.display().to_string(),
            "provider": "local",
            "identifier": null,
            "repo_cloned": false,
            "clone_origin_url": null,
            "clone_branch": null,
        }),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "run.started",
        serde_json::json!({
            "name": "Flow",
            "base_branch": "main",
            "base_sha": base_sha,
            "run_branch": format!("fabro/run/{}", run.run_id),
            "worktree_dir": context.temp_dir.display().to_string(),
            "goal": "Edit a tracked file",
        }),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "run.starting",
        serde_json::json!({}),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "run.running",
        serde_json::json!({}),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        Some("start"),
        "checkpoint.completed",
        checkpoint_properties("success", "start", &["start"], Some("step_one"), None, None),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        Some("step_one"),
        "checkpoint.completed",
        checkpoint_properties(
            "success",
            "step_one",
            &["start", "step_one"],
            Some("step_two"),
            Some(step_one_sha),
            Some(step_one_patch()),
        ),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        Some("step_two"),
        "checkpoint.completed",
        checkpoint_properties(
            "success",
            "step_two",
            &["start", "step_one", "step_two"],
            Some("exit"),
            Some(step_two_sha),
            Some(step_two_patch()),
        ),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "run.completed",
        serde_json::json!({
            "duration_ms": 456,
            "artifact_count": 0,
            "status": "success",
            "reason": "completed",
            "total_usd_micros": null,
            "final_git_commit_sha": step_two_sha,
            "final_patch": final_story_patch(),
            "billing": null,
        }),
    )
    .await;
}

async fn append_seeded_git_noop_events(
    client: &fabro_http::HttpClient,
    base_url: &str,
    run: &RunSetup,
    context: &TestContext,
    base_sha: &str,
) {
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "run.started",
        serde_json::json!({
            "name": "Flow",
            "base_branch": "main",
            "base_sha": base_sha,
            "run_branch": format!("fabro/run/{}", run.run_id),
            "worktree_dir": context.temp_dir.display().to_string(),
            "goal": "Leave tracked files unchanged",
        }),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "run.starting",
        serde_json::json!({}),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "run.running",
        serde_json::json!({}),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "run.completed",
        serde_json::json!({
            "duration_ms": 123,
            "artifact_count": 0,
            "status": "success",
            "reason": "completed",
            "total_usd_micros": null,
            "final_git_commit_sha": base_sha,
            "final_patch": null,
            "billing": null,
        }),
    )
    .await;
}

async fn append_seeded_artifact_run_events(
    client: &fabro_http::HttpClient,
    base_url: &str,
    run: &RunSetup,
    context: &TestContext,
) {
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "run.started",
        serde_json::json!({
            "name": "ArtifactRun",
            "base_branch": null,
            "base_sha": null,
            "run_branch": null,
            "worktree_dir": context.temp_dir.display().to_string(),
            "goal": "Exercise artifact commands",
        }),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "run.starting",
        serde_json::json!({}),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "run.running",
        serde_json::json!({}),
    )
    .await;
    append_run_event(
        client,
        base_url,
        &run.run_id,
        None,
        "run.completed",
        serde_json::json!({
            "duration_ms": 123,
            "artifact_count": 6,
            "status": "success",
            "reason": "completed",
            "total_usd_micros": null,
            "final_git_commit_sha": null,
            "final_patch": null,
            "billing": null,
        }),
    )
    .await;
}

async fn upload_seeded_artifact(
    client: &fabro_http::HttpClient,
    base_url: &str,
    run_id: &str,
    stage_id: &str,
    path: &str,
    contents: &str,
) {
    let response = client
        .post(format!(
            "{base_url}/api/v1/runs/{run_id}/stages/{stage_id}/artifacts?filename={path}"
        ))
        .header(fabro_http::header::CONTENT_TYPE, "application/octet-stream")
        .body(contents.to_string())
        .send()
        .await
        .unwrap_or_else(|err| panic!("seeded artifact upload should execute: {err}"));
    expect_reqwest_status(
        response,
        fabro_http::StatusCode::NO_CONTENT,
        format!("POST /api/v1/runs/{run_id}/stages/{stage_id}/artifacts ({path})"),
    )
    .await;
}

async fn append_seeded_stage(
    client: &fabro_http::HttpClient,
    base_url: &str,
    run_id: &str,
    node_id: &str,
    name: &str,
    index: usize,
    response: Option<&str>,
) {
    append_run_event(
        client,
        base_url,
        run_id,
        Some(node_id),
        "stage.started",
        serde_json::json!({
            "index": index,
            "handler_type": "noop",
            "attempt": 1,
            "max_attempts": 1,
        }),
    )
    .await;
    append_run_event(
        client,
        base_url,
        run_id,
        Some(node_id),
        "stage.completed",
        stage_completed_properties(index, response),
    )
    .await;

    let _ = name;
}

async fn append_seeded_edge(
    client: &fabro_http::HttpClient,
    base_url: &str,
    run_id: &str,
    from_node: &str,
    to_node: &str,
) {
    append_run_event(
        client,
        base_url,
        run_id,
        Some(from_node),
        "edge.selected",
        serde_json::json!({
            "from_node": from_node,
            "to_node": to_node,
            "label": null,
            "condition": null,
            "reason": "unconditional",
            "preferred_label": null,
            "suggested_next_ids": [],
            "stage_status": "success",
            "is_jump": false,
        }),
    )
    .await;
}

async fn append_run_event(
    client: &fabro_http::HttpClient,
    base_url: &str,
    run_id: &str,
    node_id: Option<&str>,
    event_name: &str,
    properties: serde_json::Value,
) {
    let event_id = NEXT_SEEDED_EVENT_ID.fetch_add(1, Ordering::Relaxed);
    let mut event = serde_json::json!({
        "id": format!("00000000-0000-0000-0000-{event_id:012x}"),
        "ts": chrono::Utc::now().to_rfc3339(),
        "run_id": run_id,
        "event": event_name,
        "properties": properties,
        "actor": {
            "kind": "system",
            "id": "worker",
            "display": "system:worker",
        },
    });
    if let Some(node_id) = node_id {
        event["node_id"] = serde_json::Value::String(node_id.to_string());
        event["node_label"] = serde_json::Value::String(node_label(node_id).to_string());
    }

    let response = client
        .post(format!("{base_url}/api/v1/runs/{run_id}/events"))
        .json(&event)
        .send()
        .await
        .unwrap_or_else(|err| panic!("append seeded event {event_name} should execute: {err}"));
    expect_reqwest_status(
        response,
        fabro_http::StatusCode::OK,
        format!("POST /api/v1/runs/{run_id}/events ({event_name})"),
    )
    .await;
}

fn test_labels(context: &TestContext) -> Vec<String> {
    vec![context.test_run_label(), context.test_case_label()]
}

fn stage_completed_properties(index: usize, response: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "index": index,
        "duration_ms": 1,
        "status": "success",
        "preferred_label": null,
        "suggested_next_ids": [],
        "billing": null,
        "failure": null,
        "notes": null,
        "files_touched": [],
        "context_updates": null,
        "jump_to_node": null,
        "context_values": null,
        "node_visits": null,
        "loop_failure_signatures": null,
        "restart_failure_signatures": null,
        "response": response,
        "attempt": 1,
        "max_attempts": 1,
    })
}

fn checkpoint_properties(
    status: &str,
    current_node: &str,
    completed_nodes: &[&str],
    next_node_id: Option<&str>,
    git_commit_sha: Option<&str>,
    diff: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "status": status,
        "current_node": current_node,
        "completed_nodes": completed_nodes,
        "node_retries": {},
        "context_values": {},
        "node_outcomes": {},
        "next_node_id": next_node_id,
        "git_commit_sha": git_commit_sha,
        "loop_failure_signatures": {},
        "restart_failure_signatures": {},
        "node_visits": {
            (current_node): 1,
        },
        "diff": diff,
    })
}

fn node_label(node_id: &str) -> &str {
    match node_id {
        "start" => "Start",
        "run_tests" => "Run Tests",
        "report" => "Report",
        "exit" => "Exit",
        "step_one" => "step_one",
        "step_two" => "step_two",
        other => other,
    }
}

fn fast_simple_workflow_source() -> &'static str {
    r#"digraph Simple {
    graph [goal="Run tests and report results"]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    run_tests [shape=parallelogram, label="Run Tests", script="true"]
    report    [shape=parallelogram, label="Report", script="true"]

    start -> run_tests -> report -> exit
}
"#
}

fn changed_git_workflow_source() -> &'static str {
    r#"digraph Flow {
  graph [goal="Edit a tracked file"];
  start [shape=Mdiamond];
  exit [shape=Msquare];
  step_one [shape=parallelogram, script="printf 'line 1\nline 2\n' > story.txt"];
  step_two [shape=parallelogram, script="printf 'line 1\nline 2\nline 3\n' > story.txt"];
  start -> step_one -> step_two -> exit;
}
"#
}

fn noop_git_workflow_source() -> &'static str {
    r#"digraph Flow {
  graph [goal="Leave tracked files unchanged"];
  start [shape=Mdiamond];
  exit [shape=Msquare];
  check [shape=parallelogram, script="test -f story.txt"];
  start -> check -> exit;
}
"#
}

fn artifact_workflow_source() -> &'static str {
    r#"digraph ArtifactRun {
  graph [goal="Exercise artifact commands", default_max_retries=0]
  start [shape=Mdiamond]
  exit [shape=Msquare]
  create_assets [shape=parallelogram, script="true", max_retries=0]
  retry_assets [shape=parallelogram, script="true", retry_policy="linear", timeout="500ms"]
  create_colliding [shape=parallelogram, script="true", max_retries=0]
  start -> create_assets -> retry_assets -> create_colliding -> exit
}
"#
}

fn step_one_patch() -> &'static str {
    "diff --git a/story.txt b/story.txt\nindex 1111111..2222222 100644\n--- a/story.txt\n+++ b/story.txt\n@@ -1 +1,2 @@\n line 1\n+line 2\n"
}

fn step_two_patch() -> &'static str {
    "diff --git a/story.txt b/story.txt\nindex 2222222..3333333 100644\n--- a/story.txt\n+++ b/story.txt\n@@ -1,2 +1,3 @@\n line 1\n line 2\n+line 3\n"
}

fn final_story_patch() -> &'static str {
    "diff --git a/story.txt b/story.txt\nindex 1111111..3333333 100644\n--- a/story.txt\n+++ b/story.txt\n@@ -1 +1,3 @@\n line 1\n+line 2\n+line 3\n"
}

pub(crate) fn text_tree(root: &Path) -> Vec<String> {
    fn visit(root: &Path, dir: &Path, entries: &mut Vec<String>) {
        let mut children: Vec<_> = std::fs::read_dir(dir)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        children.sort();

        for path in children {
            if path.is_dir() {
                visit(root, &path, entries);
                continue;
            }

            let rel = path
                .strip_prefix(root)
                .unwrap_or_else(|err| panic!("failed to strip prefix {}: {err}", root.display()))
                .display()
                .to_string();
            let contents = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
            entries.push(format!("{rel} = {contents}"));
        }
    }

    if !root.exists() {
        return Vec::new();
    }

    let mut entries = Vec::new();
    visit(root, root, &mut entries);
    entries
}

pub(crate) fn compact_inspect(output: &Output) -> Value {
    let items: Vec<Value> =
        serde_json::from_str(&stdout(output)).expect("inspect output should be valid JSON");
    Value::Array(
        items.into_iter()
            .map(|item| {
                let run_spec = item["run_spec"].clone();
                let checkpoint = item["checkpoint"].clone();
                let conclusion = item["conclusion"].clone();
                let sandbox = item["sandbox"].clone();
                let dry_run = run_spec
                    .pointer("/settings/run/execution/mode")
                    .and_then(Value::as_str)
                    .map(|mode| Value::Bool(mode == "dry_run"));
                serde_json::json!({
                    "run_id": "[ULID]",
                    "status": item["status"],
                    "run_spec": {
                        "goal": run_spec.pointer("/settings/run/goal"),
                        "workflow_name": run_spec.pointer("/graph/name"),
                        "workflow_slug": run_spec.pointer("/workflow_slug"),
                        "sandbox_provider": run_spec.pointer("/settings/run/sandbox/provider"),
                        "dry_run": dry_run,
                        "provenance": run_spec.pointer("/provenance").as_ref().map(|_| {
                            serde_json::json!({
                                "server_version": "[VERSION]",
                                "client_name": run_spec.pointer("/provenance/client/name"),
                                "client_version": "[VERSION]",
                                "subject_auth_method": run_spec.pointer("/provenance/subject/auth_method"),
                            })
                        }),
                    },
                    "start_record": item["start_record"].as_object().map(|record| {
                        serde_json::json!({
                            "has_start_time": record.contains_key("start_time"),
                        })
                    }),
                    "conclusion": conclusion.as_object().map(|_| {
                        serde_json::json!({
                            "status": conclusion["status"],
                            "duration_ms": "[DURATION_MS]",
                            "stage_count": conclusion["stages"].as_array().map(|stages| stages.len()),
                        })
                    }),
                    "checkpoint": checkpoint.as_object().map(|_| {
                        serde_json::json!({
                            "current_node": checkpoint["current_node"],
                            "completed_nodes": checkpoint["completed_nodes"],
                            "next_node_id": checkpoint["next_node_id"],
                        })
                    }),
                    "sandbox": sandbox.as_object().map(|_| {
                        serde_json::json!({
                            "provider": sandbox["provider"],
                        })
                    }),
                })
            })
            .collect(),
    )
}

pub(crate) fn compact_git_inspect(output: &Output) -> Value {
    let items: Vec<Value> =
        serde_json::from_str(&stdout(output)).expect("inspect output should be valid JSON");
    Value::Array(
        items.into_iter()
            .map(|item| {
                let run_spec = item["run_spec"].clone();
                let start_record = item["start_record"].clone();
                let checkpoint = item["checkpoint"].clone();
                let conclusion = item["conclusion"].clone();
                let sandbox = item["sandbox"].clone();
                serde_json::json!({
                    "run_id": "[ULID]",
                    "status": item["status"],
                    "run_spec": {
                        "goal": run_spec.pointer("/settings/run/goal"),
                        "workflow_name": run_spec.pointer("/graph/name"),
                        "workflow_slug": run_spec.pointer("/workflow_slug"),
                        "llm_provider": run_spec.pointer("/settings/run/model/provider"),
                        "sandbox_provider": run_spec.pointer("/settings/run/sandbox/provider"),
                        "provenance": run_spec.pointer("/provenance").as_ref().map(|_| {
                            serde_json::json!({
                                "server_version": "[VERSION]",
                                "client_name": run_spec.pointer("/provenance/client/name"),
                                "client_version": "[VERSION]",
                                "subject_auth_method": run_spec.pointer("/provenance/subject/auth_method"),
                            })
                        }),
                    },
                    "start_record": start_record.as_object().map(|_| {
                        serde_json::json!({
                            "has_start_time": true,
                            "run_branch": "fabro/run/[ULID]",
                            "base_sha": "[SHA]",
                        })
                    }),
                    "conclusion": conclusion.as_object().map(|_| {
                        serde_json::json!({
                            "status": conclusion["status"],
                            "duration_ms": "[DURATION_MS]",
                            "final_git_commit_sha": "[SHA]",
                            "stage_count": conclusion["stages"].as_array().map(|stages| stages.len()),
                        })
                    }),
                    "checkpoint": checkpoint.as_object().map(|_| {
                        serde_json::json!({
                            "current_node": checkpoint["current_node"],
                            "completed_nodes": checkpoint["completed_nodes"],
                            "next_node_id": checkpoint["next_node_id"],
                            "git_commit_sha": "[SHA]",
                        })
                    }),
                    "sandbox": sandbox.as_object().map(|_| {
                        serde_json::json!({
                            "provider": sandbox["provider"],
                            "working_directory": "[WORKTREE]",
                        })
                    }),
                })
            })
            .collect(),
    )
}

fn write_text_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|err| panic!("failed to create {}: {err}", parent.display()));
    }
    std::fs::write(path, content)
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", path.display()));
}

fn to_pascal_case(s: &str) -> String {
    s.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let upper: String = first.to_uppercase().collect();
                    format!("{upper}{rest}", rest = chars.as_str())
                }
                None => String::new(),
            }
        })
        .collect()
}
