#![allow(
    clippy::absolute_paths,
    clippy::manual_assert,
    clippy::redundant_closure_for_method_calls
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use fabro_config::Storage;
use fabro_server::bind::Bind;
use fabro_store::EventEnvelope;
use fabro_test::TestContext;
use fabro_types::RunId;
use serde_json::Value;
use shlex::try_quote;

use crate::support::unique_run_id;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

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

pub(crate) struct GitRunSetup {
    pub(crate) run:      RunSetup,
    pub(crate) repo_dir: PathBuf,
    pub(crate) base_sha: String,
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
enum GitWorkflowKind {
    Changed,
    Noop,
}

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
    cmd.timeout(COMMAND_TIMEOUT);
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
    let workflow = fixture("simple.fabro");
    run_completed_dry_run(context, &workflow)
}

pub(crate) fn setup_completed_fast_dry_run(context: &TestContext) -> RunSetup {
    let workflow = fast_simple_workflow(context);
    run_completed_dry_run(context, &workflow)
}

fn run_completed_dry_run(context: &TestContext, workflow: &Path) -> RunSetup {
    let run_id = unique_run_id();
    let mut cmd = context.run_cmd();
    cmd.current_dir(&context.temp_dir);
    cmd.timeout(COMMAND_TIMEOUT);
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

pub(crate) fn setup_created_dry_run(context: &TestContext) -> RunSetup {
    let workflow = fixture("simple.fabro");
    run_created_dry_run(context, &workflow)
}

pub(crate) fn setup_created_fast_dry_run(context: &TestContext) -> RunSetup {
    let workflow = fast_simple_workflow(context);
    run_created_dry_run(context, &workflow)
}

fn run_created_dry_run(context: &TestContext, workflow: &Path) -> RunSetup {
    let run_id = unique_run_id();
    let mut cmd = context.create_cmd();
    cmd.current_dir(&context.temp_dir);
    cmd.timeout(COMMAND_TIMEOUT);
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
            "command failed: fabro create --dry-run --auto-approve --no-retro --sandbox local {}\nstdout:\n{}\nstderr:\n{}",
            workflow.display(),
            stdout(&output),
            stderr(&output)
        );
    }
    assert_eq!(stdout(&output).trim(), run_id);
    RunSetup {
        run_dir: context.find_run_dir(&run_id),
        run_id,
    }
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
    let workflow = fixture("simple.fabro");
    let run_id = unique_run_id();
    let mut cmd = context.run_cmd();
    cmd.current_dir(&context.temp_dir);
    cmd.timeout(COMMAND_TIMEOUT);
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
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    while run_events(&run.run_dir).is_empty() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for store events for {run_id}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    run
}

pub(crate) fn setup_git_backed_changed_run(context: &TestContext) -> GitRunSetup {
    setup_git_backed_run(context, GitWorkflowKind::Changed)
}

pub(crate) fn setup_git_backed_noop_run(context: &TestContext) -> GitRunSetup {
    setup_git_backed_run(context, GitWorkflowKind::Noop)
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

pub(crate) fn setup_artifact_run(context: &TestContext) -> WorkspaceRunSetup {
    let workspace_dir = context.temp_dir.join("artifact-run");
    std::fs::create_dir_all(&workspace_dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", workspace_dir.display()));

    write_text_file(
        &workspace_dir.join("artifact_run.fabro"),
        r#"digraph ArtifactRun {
  graph [goal="Exercise artifact commands", default_max_retries=0]
  start [shape=Mdiamond]
  exit [shape=Msquare]
  create_assets [shape=parallelogram, script="mkdir -p assets/shared assets/node_a && printf one > assets/shared/report.txt && printf alpha > assets/node_a/summary.txt", max_retries=0]
  retry_assets [shape=parallelogram, script="mkdir -p assets/retry && touch -c -t 200001010000 assets/shared/report.txt assets/node_a/summary.txt && if [ ! -f .retry-sentinel ]; then printf first > assets/retry/report.txt && touch .retry-sentinel && sleep 1; else printf second > assets/retry/report.txt; fi", retry_policy="linear", timeout="500ms"]
  create_colliding [shape=parallelogram, script="mkdir -p assets/other assets/retry && touch -c -t 200001010000 assets/shared/report.txt assets/node_a/summary.txt assets/retry/report.txt && printf beta > assets/other/summary.txt && printf second > assets/retry/report.txt", max_retries=0]
  start -> create_assets -> retry_assets -> create_colliding -> exit
}
"#,
    );
    write_text_file(
        &workspace_dir.join("run.toml"),
        r#"_version = 1

[workflow]
graph = "artifact_run.fabro"

[run]
goal = "Exercise artifact commands"

[run.sandbox]
provider = "local"
preserve = true

[run.sandbox.local]
worktree_mode = "never"

[run.artifacts]
include = ["assets/**"]
"#,
    );

    let run = run_local_workflow(context, &workspace_dir, "run.toml");

    WorkspaceRunSetup { run, workspace_dir }
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
    cmd.timeout(COMMAND_TIMEOUT);
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
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        if let Some(status) = run_state(run_dir)
            .status
            .map(|record| record.status.to_string())
        {
            if expected.iter().any(|candidate| *candidate == status) {
                return status;
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
    filters
}

#[expect(
    clippy::disallowed_methods,
    reason = "This sync integration helper polls for the run directory to appear without requiring a Tokio runtime."
)]
pub(crate) fn resolve_run(context: &TestContext, run_id: &str) -> RunSetup {
    let deadline = Instant::now() + COMMAND_TIMEOUT;
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
        .unwrap()
        .block_on(future)
}

#[derive(Debug, serde::Deserialize)]
struct TestServerRecord {
    bind: Bind,
}

fn server_endpoint(storage_dir: &Path) -> Option<(reqwest::Client, String)> {
    let record_path = Storage::new(storage_dir).server_state().record_path();
    let record = std::fs::read_to_string(record_path)
        .ok()
        .and_then(|content| serde_json::from_str::<TestServerRecord>(&content).ok())?;
    match record.bind {
        Bind::Unix(path) if path.exists() => Some((
            reqwest::ClientBuilder::new()
                .unix_socket(path)
                .no_proxy()
                .build()
                .expect("test Unix-socket HTTP client should build"),
            "http://fabro".to_string(),
        )),
        Bind::Unix(_) => None,
        Bind::Tcp(addr) => Some((
            reqwest::ClientBuilder::new()
                .no_proxy()
                .build()
                .expect("test TCP HTTP client should build"),
            format!("http://{addr}"),
        )),
    }
}

pub(crate) fn server_target(storage_dir: &Path) -> String {
    let record_path = Storage::new(storage_dir).server_state().record_path();
    let record = std::fs::read_to_string(record_path)
        .ok()
        .and_then(|content| serde_json::from_str::<TestServerRecord>(&content).ok())
        .expect("server record should exist");
    match record.bind {
        Bind::Unix(path) => path.to_string_lossy().to_string(),
        Bind::Tcp(addr) => format!("http://{addr}"),
    }
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
    if !response.status().is_success() {
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
    assert!(
        response.status().is_success(),
        "server request failed for {path}: {}",
        response.status()
    );
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
    let deadline = std::time::Instant::now() + COMMAND_TIMEOUT;

    loop {
        let event_names = run_events(run_dir)
            .into_iter()
            .filter_map(|event| {
                event
                    .payload
                    .as_value()
                    .get("event")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string)
            })
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

pub(crate) fn git_stdout(repo_dir: &Path, args: &[&str]) -> String {
    stdout(&git_success(repo_dir, args))
}

pub(crate) fn metadata_run_ids(repo_dir: &Path) -> BTreeSet<String> {
    git_stdout(repo_dir, &["branch", "--format=%(refname:short)"])
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("fabro/meta/"))
        .map(ToOwned::to_owned)
        .collect()
}

pub(crate) fn run_branch_commits(repo_dir: &Path, run_id: &str) -> Vec<String> {
    git_stdout(repo_dir, &[
        "rev-list",
        "--reverse",
        &format!("fabro/run/{run_id}"),
    ])
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty())
    .map(ToOwned::to_owned)
    .collect()
}

pub(crate) fn run_branch_commits_since_base(
    repo_dir: &Path,
    run_id: &str,
    base_sha: &str,
) -> Vec<String> {
    git_stdout(repo_dir, &[
        "rev-list",
        "--reverse",
        &format!("{base_sha}..fabro/run/{run_id}"),
    ])
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty())
    .map(ToOwned::to_owned)
    .collect()
}

pub(crate) fn git_show_json(repo_dir: &Path, revspec: &str) -> Value {
    let output = git_success(repo_dir, &["show", revspec]);
    serde_json::from_str(&stdout(&output))
        .unwrap_or_else(|err| panic!("failed to parse git show {revspec}: {err}"))
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
                let run_record = item["run_record"].clone();
                let checkpoint = item["checkpoint"].clone();
                let conclusion = item["conclusion"].clone();
                let sandbox = item["sandbox"].clone();
                let dry_run = run_record
                    .pointer("/settings/run/execution/mode")
                    .and_then(Value::as_str)
                    .map(|mode| Value::Bool(mode == "dry_run"));
                serde_json::json!({
                    "run_id": "[ULID]",
                    "status": item["status"],
                    "run_record": {
                        "goal": run_record.pointer("/settings/run/goal"),
                        "workflow_name": run_record.pointer("/graph/name"),
                        "workflow_slug": run_record.pointer("/workflow_slug"),
                        "sandbox_provider": run_record.pointer("/settings/run/sandbox/provider"),
                        "dry_run": dry_run,
                        "provenance": run_record.pointer("/provenance").as_ref().map(|_| {
                            serde_json::json!({
                                "server_version": "[VERSION]",
                                "client_name": run_record.pointer("/provenance/client/name"),
                                "client_version": "[VERSION]",
                                "subject_auth_method": run_record.pointer("/provenance/subject/auth_method"),
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
                let run_record = item["run_record"].clone();
                let start_record = item["start_record"].clone();
                let checkpoint = item["checkpoint"].clone();
                let conclusion = item["conclusion"].clone();
                let sandbox = item["sandbox"].clone();
                serde_json::json!({
                    "run_id": "[ULID]",
                    "status": item["status"],
                    "run_record": {
                        "goal": run_record.pointer("/settings/run/goal"),
                        "workflow_name": run_record.pointer("/graph/name"),
                        "workflow_slug": run_record.pointer("/workflow_slug"),
                        "llm_provider": run_record.pointer("/settings/run/model/provider"),
                        "sandbox_provider": run_record.pointer("/settings/run/sandbox/provider"),
                        "provenance": run_record.pointer("/provenance").as_ref().map(|_| {
                            serde_json::json!({
                                "server_version": "[VERSION]",
                                "client_name": run_record.pointer("/provenance/client/name"),
                                "client_version": "[VERSION]",
                                "subject_auth_method": run_record.pointer("/provenance/subject/auth_method"),
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

fn setup_git_backed_run(context: &TestContext, workflow: GitWorkflowKind) -> GitRunSetup {
    let repo_dir = context.temp_dir.join(match workflow {
        GitWorkflowKind::Changed => "git-changed",
        GitWorkflowKind::Noop => "git-noop",
    });
    std::fs::create_dir_all(&repo_dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", repo_dir.display()));

    git_success(&repo_dir, &["init", "-q"]);
    git_success(&repo_dir, &["config", "user.name", "Fabro Test"]);
    git_success(&repo_dir, &["config", "user.email", "test@example.com"]);

    write_text_file(&repo_dir.join("story.txt"), "line 1\n");
    write_text_file(&repo_dir.join("flow.fabro"), match workflow {
        GitWorkflowKind::Changed => {
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
        GitWorkflowKind::Noop => {
            r#"digraph Flow {
  graph [goal="Leave tracked files unchanged"];
  start [shape=Mdiamond];
  exit [shape=Msquare];
  check [shape=parallelogram, script="test -f story.txt"];
  start -> check -> exit;
}
"#
        }
    });

    git_success(&repo_dir, &["add", "story.txt", "flow.fabro"]);
    git_success(&repo_dir, &["commit", "-qm", "init"]);
    let base_sha = git_stdout(&repo_dir, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let run_id = unique_run_id();

    let mut cmd = context.run_cmd();
    cmd.current_dir(&repo_dir);
    cmd.env("OPENAI_API_KEY", "test");
    cmd.args([
        "--run-id",
        run_id.as_str(),
        "--sandbox",
        "local",
        "--no-retro",
        "--provider",
        "openai",
        "flow.fabro",
    ]);
    let output = cmd.output().expect("command should execute");
    if !output.status.success() {
        panic!(
            "command failed: fabro run --sandbox local --no-retro --provider openai flow.fabro\nstdout:\n{}\nstderr:\n{}",
            stdout(&output),
            stderr(&output)
        );
    }

    let run = RunSetup {
        run_dir: context.find_run_dir(&run_id),
        run_id,
    };
    let start = serde_json::to_value(
        run_state(&run.run_dir)
            .start
            .expect("start record should exist"),
    )
    .unwrap();
    assert_eq!(
        start["run_branch"].as_str(),
        Some(format!("fabro/run/{}", run.run_id).as_str())
    );
    assert_eq!(start["base_sha"].as_str(), Some(base_sha.as_str()));
    match workflow {
        GitWorkflowKind::Changed => {
            assert!(
                run_state(&run.run_dir).final_patch.is_some(),
                "changed git-backed run should persist final patch in store"
            );
            let state = run_state(&run.run_dir);
            assert!(
                state
                    .iter_nodes()
                    .any(|(node, state)| node.node_id() == "step_one" && state.diff.is_some())
            );
            assert!(
                state
                    .iter_nodes()
                    .any(|(node, state)| node.node_id() == "step_two" && state.diff.is_some())
            );
        }
        GitWorkflowKind::Noop => {
            assert!(
                run_state(&run.run_dir).final_patch.is_none(),
                "no-op git-backed run should not persist final.patch"
            );
        }
    }

    GitRunSetup {
        run,
        repo_dir,
        base_sha,
    }
}

fn git_success(repo_dir: &Path, args: &[&str]) -> Output {
    let output = std::process::Command::new("git")
        .current_dir(repo_dir)
        .args(args)
        .output()
        .expect("git command should execute");
    if !output.status.success() {
        panic!(
            "git command failed: git {}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            stdout(&output),
            stderr(&output)
        );
    }
    output
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
