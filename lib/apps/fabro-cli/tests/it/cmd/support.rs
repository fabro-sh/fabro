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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use fabro_client::Client;
use fabro_config::bind::Bind;
use fabro_config::daemon::ServerDaemon;
use fabro_config::{Storage, envfile};
use fabro_test::{TestContext, expect_reqwest_status};
use fabro_types::test_support::test_principal;
use fabro_types::{
    GitRunTarget, RunId, RunIntent, RunIntentArgs, RunStreamItem, RunTarget, StageId, WorkflowPath,
    WorkflowVersion,
};
use httpmock::{HttpMockResponse, Mock, MockServer};
use serde_json::Value;
use shlex::try_quote;

const LOCAL_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const CI_COMMAND_TIMEOUT: Duration = Duration::from_secs(90);

pub(crate) use fabro_store::RunProjection;

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct RunSummaryRecord {
    run_id: String,
    #[serde(default)]
    labels: std::collections::HashMap<String, String>,
}

#[derive(Debug, serde::Deserialize)]
struct CommandLogResponseRecord {
    bytes_base64: String,
}

pub(crate) struct RunSetup {
    pub(crate) run_id:  String,
    pub(crate) run_dir: PathBuf,
}

pub(crate) struct ProjectFixture {
    pub(crate) project_dir: PathBuf,
    pub(crate) fabro_root:  PathBuf,
}

/// A run whose workflow populated its sandbox. The run executes in its own
/// workspace, not in the target folder, so the sandbox's files are read
/// back through the run.
pub(crate) struct WorkspaceRunSetup {
    pub(crate) run: RunSetup,
}

pub(crate) struct WorkflowGate {
    gate_path: PathBuf,
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

pub(crate) fn created_run_id(output: &Output) -> String {
    stdout(output)
        .trim()
        .parse::<RunId>()
        .expect("create command should print a run ID")
        .to_string()
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
            .json_body(remote_run_summary_json(
                run_id,
                "Nightly Build",
                "nightly-build",
                "Nightly run",
                &serde_json::json!({
                    "kind": "succeeded",
                    "reason": "completed"
                }),
                "2026-04-05T12:00:00Z",
            ));
    })
}

/// Canonical environment response body for mock servers, matching the
/// `GET /api/v1/environments/{id}` shape the run-intent create path reads.
pub(crate) fn environment_json(id: &str, provider: &str) -> Value {
    serde_json::json!({
        "id": id,
        "revision": "0".repeat(64),
        "provider": provider,
        "image": { "docker": null, "dockerfile": null },
        "resources": { "cpu": null, "memory": null, "disk": null },
        "network": { "mode": "allow_all", "allow": [] },
        "lifecycle": {
            "preserve": false,
            "stop_on_terminal": true,
            "auto_stop": null
        },
        "labels": {},
        "env": {}
    })
}

pub(crate) fn mock_environment<'a>(server: &'a MockServer, id: &str, provider: &str) -> Mock<'a> {
    server.mock(|when, then| {
        when.method("GET")
            .path(format!("/api/v1/environments/{id}"));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(environment_json(id, provider));
    })
}

pub(crate) fn mock_workflow_version_registrations(server: &MockServer) -> Mock<'_> {
    mock_workflow_version_registrations_recording(server, Arc::new(Mutex::new(Vec::new())))
}

/// Accepts `POST /api/v1/workflow-versions`, echoing each version's
/// content-derived ID back, and records every request body into
/// `registrations` for later assertions.
pub(crate) fn mock_workflow_version_registrations_recording(
    server: &MockServer,
    registrations: Arc<Mutex<Vec<Value>>>,
) -> Mock<'_> {
    server.mock(|when, then| {
        when.method("POST").path("/api/v1/workflow-versions");
        then.respond_with(move |request| {
            let body: Value = serde_json::from_slice(request.body_ref())
                .expect("workflow-version request body should be valid JSON");
            let version: fabro_types::WorkflowVersion = serde_json::from_value(body.clone())
                .expect("workflow-version request body should be a workflow version");
            registrations.lock().unwrap().push(body);
            HttpMockResponse::builder()
                .status(201)
                .header("content-type", "application/json")
                .body(
                    serde_json::json!({
                        "workflow_version_id": version
                            .id()
                            .expect("mocked workflow version should have a valid ID")
                    })
                    .to_string(),
                )
                .build()
        });
    })
}

/// Runs a `git` command in `path` for fixture setup, panicking on failure and
/// returning trimmed stdout.
/// Write a minimal `workflow.toml` and `workflow.fabro` pair under
/// `root/directory`; `graph_name` distinguishes fixtures by content.
pub(crate) fn write_workflow(root: &Path, directory: &str, graph_name: &str) -> PathBuf {
    let directory = root.join(directory);
    std::fs::create_dir_all(&directory).expect("workflow fixture directory should be created");
    std::fs::write(
        directory.join("workflow.toml"),
        "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
    )
    .expect("workflow fixture manifest should be written");
    std::fs::write(
        directory.join("workflow.fabro"),
        format!(
            "digraph {graph_name} {{ start [shape=Mdiamond] exit [shape=Msquare] start -> exit }}"
        ),
    )
    .expect("workflow fixture graph should be written");
    directory.join("workflow.toml")
}

/// Initialize `path` as a repository on `branch` with one commit of its
/// current contents, returning the commit SHA.
pub(crate) fn init_remote_fixture(path: &Path, branch: &str) -> String {
    let repo = git2::Repository::init_opts(
        path,
        git2::RepositoryInitOptions::new().initial_head(branch),
    )
    .expect("fixture repository should initialize");
    let mut index = repo.index().expect("fixture index should open");
    index
        .add_all(["."], git2::IndexAddOption::DEFAULT, None)
        .expect("fixture files should stage");
    let tree_id = index.write_tree().expect("fixture tree should write");
    let tree = repo.find_tree(tree_id).expect("fixture tree should exist");
    let signature = git2::Signature::now("Fixture", "fixture@example.test")
        .expect("fixture signature should be valid");
    repo.commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
        .expect("fixture commit should succeed")
        .to_string()
}

pub(crate) fn run_git(path: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("Git fixture command should execute");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git fixture output should be UTF-8")
        .trim()
        .to_string()
}

/// Snapshot filter that scrubs short (12-char) ULID suffixes from output, used
/// when the CLI prints abbreviated run IDs.
pub(crate) fn ulid_filter() -> (String, String) {
    (
        r"\b[0-9A-HJKMNP-TV-Z]{12}\b".to_string(),
        "[ULID]".to_string(),
    )
}

/// JSON 409 error body mirroring the server's batch-error shape, used by mock
/// HTTP servers in CLI tests that exercise partial-failure code paths.
pub(crate) fn conflict_error_body(detail: &str) -> Value {
    serde_json::json!({
        "errors": [{
            "status": "409",
            "title": "Conflict",
            "detail": detail,
        }]
    })
}

pub(crate) fn remote_run_summary_json(
    run_id: &str,
    workflow_name: &str,
    workflow_slug: &str,
    goal: &str,
    status: &Value,
    timestamp: &str,
) -> Value {
    serde_json::json!({
        "id": run_id,
        "title": goal,
        "goal": goal,
        "workflow": {
            "slug": workflow_slug,
            "name": workflow_name,
            "graph_name": null
        },
        "repository": {
            "name": "repo",
            "origin_url": null,
            "provider": "unknown"
        },
        "created_by": serde_json::to_value(test_principal())
            .expect("test principal should serialize"),
        "origin": {
            "kind": "api"
        },
        "labels": {},
        "lifecycle": {
            "status": status,
            "pending_control": null,
            "queue_position": null,
            "error": null,
            "archived": false,
            "archived_at": null
        },
        "models": [],
        "source_directory": "/srv/repo",
        "timestamps": {
            "created_at": timestamp,
            "started_at": timestamp,
            "last_event_at": null,
            "completed_at": null
        },
        "timing": null,
        "usage": {"tokens": {"input": 0, "output": 0, "reasoning": 0, "cache_read": 0, "cache_write": 0}},
        "diff": null,
        "pull_request": null,
        "current_question": null,
        "superseded_by": null,
        "links": {
            "web": null
        }
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

/// A completed run of the fast simple workflow: a real dry run, since a
/// run's history is what the engine recorded.
pub(crate) fn setup_seeded_completed_dry_run(context: &TestContext) -> RunSetup {
    setup_completed_fast_dry_run(context)
}

pub(crate) fn setup_seeded_created_dry_run(context: &TestContext) -> RunSetup {
    block_on(seed_dry_run(context))
}

fn run_completed_dry_run(context: &TestContext, workflow: &Path) -> RunSetup {
    let mut cmd = context.run_cmd();
    cmd.current_dir(&context.temp_dir);
    cmd.timeout(command_timeout());
    cmd.args(["--dry-run", "--auto-approve", "--environment", "local"]);
    cmd.arg(workflow);
    let output = cmd.output().expect("command should execute");
    if !output.status.success() {
        panic!(
            "command failed: fabro run --dry-run --auto-approve --environment local {}\nstdout:\n{}\nstderr:\n{}",
            workflow.display(),
            stdout(&output),
            stderr(&output)
        );
    }
    let run_id = run_id_from_run_output(&output);
    let run_setup = RunSetup {
        run_dir: context.find_run_dir(&run_id),
        run_id,
    };
    wait_for_run_finished(&run_setup.run_dir);
    run_setup
}

/// The run id `fabro run` prints (`Run: <id>`) for the run it created.
fn run_id_from_run_output(output: &Output) -> String {
    let text = stderr(output);
    text.lines()
        .find_map(|line| line.trim().strip_prefix("Run: "))
        .map_or_else(
            || panic!("fabro run should print the run id:\n{text}"),
            str::trim,
        )
        .to_string()
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
    let mut cmd = context.run_cmd();
    cmd.current_dir(&context.temp_dir);
    cmd.timeout(command_timeout());
    cmd.args([
        "--detach",
        "--dry-run",
        "--auto-approve",
        "--environment",
        "local",
    ]);
    cmd.arg(workflow);
    let output = cmd.output().expect("command should execute");
    if !output.status.success() {
        panic!(
            "command failed: fabro run --detach --dry-run --auto-approve --environment local {}\nstdout:\n{}\nstderr:\n{}",
            fixture("simple.fabro").display(),
            stdout(&output),
            stderr(&output)
        );
    }
    let run_id = created_run_id(&output);
    let run = resolve_run(context, &run_id);
    let deadline = Instant::now() + command_timeout();
    while run_stream_items(&run.run_dir).is_empty() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for store events for {run_id}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    run
}

pub(crate) fn setup_seeded_artifact_run(context: &TestContext) -> RunSetup {
    seed_artifact_run(context)
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

/// A git-backed workspace whose run appends two lines to `story.txt`, one
/// per stage: `step_one` adds `line 2`, `step_two` adds `line 3`.
pub(crate) fn setup_git_backed_changed_run(context: &TestContext) -> WorkspaceRunSetup {
    git_backed_run(
        context,
        "changed",
        "step_one [shape=parallelogram, script=\"printf 'line 2\\n' >> story.txt\"]\n  \
         step_two [shape=parallelogram, script=\"printf 'line 3\\n' >> story.txt\"]",
        "start -> step_one -> step_two -> exit",
    )
}

/// A git-backed workspace whose run changes nothing.
pub(crate) fn setup_git_backed_noop_run(context: &TestContext) -> WorkspaceRunSetup {
    git_backed_run(
        context,
        "noop",
        "step_one [shape=parallelogram, script=\"cat story.txt\"]",
        "start -> step_one -> exit",
    )
}

/// A run on the local provider from a workspace with one commit, so the
/// run branch starts from a base the run's diff is measured against.
fn git_backed_run(
    context: &TestContext,
    name: &str,
    stages: &str,
    edges: &str,
) -> WorkspaceRunSetup {
    let workspace_dir = context.temp_dir.join(format!("git-{name}"));
    std::fs::create_dir_all(&workspace_dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", workspace_dir.display()));
    write_text_file(&workspace_dir.join("story.txt"), "line 1\n");
    write_text_file(
        &workspace_dir.join("story.fabro"),
        &format!(
            "digraph Story {{\n  graph [goal=\"Change the story\", default_max_retries=0]\n  \
             start [shape=Mdiamond]\n  exit [shape=Msquare]\n  {stages}\n  {edges}\n}}\n"
        ),
    );
    write_text_file(
        &workspace_dir.join("workflow.toml"),
        "_version = 1\n\n[workflow]\ngraph = \"story.fabro\"\n\n[run]\ngoal = \"Change the \
         story\"\n\n[run.environment]\nid = \"local\"\n",
    );
    init_remote_fixture(&workspace_dir, "main");
    let run = run_local_workflow(context, &workspace_dir, "workflow.toml");
    WorkspaceRunSetup { run }
}

/// The run output filters plus one for commit shas, which a patch names in
/// its index lines.
pub(crate) fn git_filters(context: &TestContext) -> Vec<(String, String)> {
    let mut filters = context.filters();
    filters.push((r"\b[0-9a-f]{7,40}\b".to_string(), "[SHA]".to_string()));
    filters
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
        &workspace_dir.join("workflow.toml"),
        r#"_version = 1

[workflow]
graph = "sandbox_run.fabro"

[run]
goal = "Exercise sandbox commands"

[run.environment]
id = "local"
"#,
    );

    let run = run_local_workflow(context, &workspace_dir, "workflow.toml");
    assert!(run_state(&run.run_dir).sandbox.is_some());

    WorkspaceRunSetup { run }
}

fn run_local_workflow(context: &TestContext, workspace_dir: &Path, workflow: &str) -> RunSetup {
    let mut cmd = context.run_cmd();
    cmd.current_dir(workspace_dir);
    cmd.timeout(command_timeout());
    cmd.env("OPENAI_API_KEY", "test");
    cmd.args([
        "--auto-approve",
        "--environment",
        "local",
        "--provider",
        "openai",
        workflow,
    ]);
    let output = cmd.output().expect("command should execute");
    if !output.status.success() {
        panic!(
            "command failed: fabro run --auto-approve --environment local --provider openai {workflow}\nstdout:\n{}\nstderr:\n{}",
            stdout(&output),
            stderr(&output)
        );
    }

    single_run_setup(context)
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
        let state = run_state(run_dir);
        let status = if state.archived_at.is_some() {
            "archived"
        } else {
            match state.status {
                fabro_types::RunStatus::Submitted => "submitted",
                fabro_types::RunStatus::Pending { .. } => "pending",
                fabro_types::RunStatus::Runnable => "runnable",
                fabro_types::RunStatus::Starting => "starting",
                fabro_types::RunStatus::Running => "running",
                fabro_types::RunStatus::Blocked { .. } => "blocked",
                fabro_types::RunStatus::Paused { .. } => "paused",
                fabro_types::RunStatus::Removing => "removing",
                fabro_types::RunStatus::Succeeded { .. } => "succeeded",
                fabro_types::RunStatus::Failed { .. } => "failed",
                fabro_types::RunStatus::Dead => "dead",
            }
        };
        if expected.contains(&status) {
            return status.to_string();
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

fn single_run_setup(context: &TestContext) -> RunSetup {
    let run_dir = context.single_run_dir();
    let run_id = infer_run_id(&run_dir);
    RunSetup { run_id, run_dir }
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
    headers.insert(
        fabro_http::header::USER_AGENT,
        fabro_http::HeaderValue::from_static("fabro-cli/test"),
    );
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

pub(crate) fn run_stream_items(run_dir: &Path) -> Vec<RunStreamItem> {
    let run_id = infer_run_id(run_dir);
    let response: serde_json::Value = block_on(get_server_json(
        run_dir,
        &format!("/api/v1/runs/{run_id}/events?after=0&limit=1000"),
    ));
    crate::support::parse_stream_items(&response)
}

pub(crate) fn command_log_text(run_dir: &Path, stage_id: &StageId) -> String {
    let run_id = infer_run_id(run_dir);
    let response: CommandLogResponseRecord = block_on(get_server_json(
        run_dir,
        &format!("/api/v1/runs/{run_id}/stages/{stage_id}/logs/output?offset=0&limit=1048576"),
    ));
    let bytes = BASE64_STANDARD
        .decode(&response.bytes_base64)
        .expect("command log bytes should decode");
    String::from_utf8(bytes).expect("command log should be UTF-8")
}

/// Wait until the run's stream holds the terminal lifecycle record.
pub(crate) fn wait_for_run_finished(run_dir: &Path) {
    wait_for_stream_item(run_dir, "the terminal lifecycle record", |item| {
        crate::support::is_terminal_lifecycle(item)
    });
}

/// Wait until the run's stream holds the `run.lifecycle` record of
/// `transition` (`running`, `succeeded`, ...).
pub(crate) fn wait_for_lifecycle(run_dir: &Path, transition: &str) {
    wait_for_stream_item(
        run_dir,
        &format!("the {transition} lifecycle record"),
        |item| {
            let record = item.item.get("record");
            record
                .and_then(|record| record.get("kind"))
                .and_then(serde_json::Value::as_str)
                == Some("run.lifecycle")
                && record
                    .and_then(|record| record.get("transition"))
                    .and_then(serde_json::Value::as_str)
                    == Some(transition)
        },
    );
}

#[expect(
    clippy::disallowed_methods,
    reason = "This sync integration helper polls the run stream without requiring a Tokio runtime."
)]
fn wait_for_stream_item(run_dir: &Path, what: &str, matches: impl Fn(&RunStreamItem) -> bool) {
    let deadline = std::time::Instant::now() + command_timeout();
    loop {
        if run_stream_items(run_dir).iter().any(&matches) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what} in {}",
            run_dir.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// A created, unstarted dry run of the fast simple workflow.
async fn seed_dry_run(context: &TestContext) -> RunSetup {
    create_seeded_run(
        context,
        "simple.fabro",
        fast_simple_workflow_source(),
        RunIntentArgs {
            dry_run: Some(true),
            auto_approve: Some(true),
            labels: test_label_map(context),
            ..Default::default()
        },
        false,
    )
    .await
}

/// A completed dry run of the artifact workflow, with artifacts uploaded
/// for its stages through the API.
fn seed_artifact_run(context: &TestContext) -> RunSetup {
    let workflow = context.temp_dir.join("artifact_run.fabro");
    write_text_file(&workflow, artifact_workflow_source());
    let run = run_completed_dry_run(context, &workflow);

    let (client, base_url) = server_endpoint(&context.storage_dir)
        .expect("test server endpoint should be available for seeded artifacts");
    block_on(async {
        for (stage_id, retry, path, contents) in [
            ("create_assets@1", 1, "assets/node_a/summary.txt", "alpha"),
            ("create_assets@1", 1, "assets/shared/report.txt", "one"),
            ("create_assets@2", 1, "assets/shared/report.txt", "two"),
            ("create_colliding@1", 1, "assets/other/summary.txt", "beta"),
            ("create_colliding@1", 1, "assets/retry/report.txt", "second"),
            ("retry_assets@1", 1, "assets/retry/report.txt", "first"),
            ("retry_assets@1", 2, "assets/retry/report.txt", "second"),
        ] {
            upload_seeded_artifact(
                &client,
                &base_url,
                &run.run_id,
                stage_id,
                retry,
                path,
                contents,
            )
            .await;
        }
    });

    run
}

async fn create_seeded_run(
    context: &TestContext,
    target_path: &str,
    source: &str,
    args: RunIntentArgs,
    git: bool,
) -> RunSetup {
    let target = if git {
        let sha = init_remote_fixture(&context.temp_dir, "main");
        run_git(&context.temp_dir, &[
            "remote",
            "add",
            "origin",
            "https://github.com/fabro-sh/seeded-fixture.git",
        ]);
        RunTarget::Git(GitRunTarget {
            repo:   "fabro-sh/seeded-fixture".to_string(),
            branch: "main".to_string(),
            sha:    Some(sha),
            tag:    None,
        })
    } else {
        RunTarget::None {}
    };
    let (client, base_url) = server_endpoint(&context.storage_dir)
        .expect("test server endpoint should be available for seeded run creation");
    let client = Client::from_http_client(base_url, client);
    let path = WorkflowPath::new(target_path).expect("seeded workflow path should be valid");
    let version = WorkflowVersion::new(
        path.clone(),
        BTreeMap::from([(path, source.to_string())]),
        BTreeMap::new(),
    )
    .expect("seeded workflow should be valid");
    let workflow_version_id = client
        .create_workflow_version(&version)
        .await
        .expect("seeded workflow registration should succeed");
    let run_id = client
        .create_run_from_intent(RunIntent {
            workflow_version_id,
            target,
            environment_id: Some("default".to_string()),
            args,
            parent_id: None,
            title: None,
            goal: None,
        })
        .await
        .expect("seeded run creation should succeed")
        .to_string();

    RunSetup {
        run_dir: context.find_run_dir(&run_id),
        run_id,
    }
}

async fn upload_seeded_artifact(
    client: &fabro_http::HttpClient,
    base_url: &str,
    run_id: &str,
    stage_id: &str,
    retry: u32,
    path: &str,
    contents: &str,
) {
    let response = client
        .post(format!(
            "{base_url}/api/v1/runs/{run_id}/stages/{stage_id}/artifacts?filename={path}&retry={retry}"
        ))
        .header(fabro_http::header::CONTENT_TYPE, "application/octet-stream")
        .body(contents.to_string())
        .send()
        .await
        .unwrap_or_else(|err| panic!("seeded artifact upload should execute: {err}"));
    expect_reqwest_status(
        response,
        fabro_http::StatusCode::NO_CONTENT,
        format!("POST /api/v1/runs/{run_id}/stages/{stage_id}/artifacts ({path}, retry {retry})"),
    )
    .await;
}

fn test_label_map(context: &TestContext) -> std::collections::HashMap<String, String> {
    test_labels(context)
        .into_iter()
        .map(|label| {
            let (key, value) = label
                .split_once('=')
                .expect("test labels should contain a key and value");
            (key.to_string(), value.to_string())
        })
        .collect()
}

fn test_labels(context: &TestContext) -> Vec<String> {
    vec![context.test_run_label(), context.test_case_label()]
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
                            "timing": "[TIMING]",
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
                            "provider": compact_sandbox_provider(&sandbox),
                        })
                    }),
                })
            })
            .collect(),
    )
}

fn compact_sandbox_provider(sandbox: &Value) -> Value {
    sandbox
        .pointer("/instance/provider")
        .or_else(|| sandbox.pointer("/plan/provider"))
        .or_else(|| sandbox.pointer("/failure/provider"))
        .or_else(|| sandbox.get("provider"))
        .cloned()
        .unwrap_or(Value::Null)
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
