use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use assert_cmd::Command;
use chrono::TimeZone;
use fabro_config::FabroSettings;
use fabro_config::mcp::McpTransport;
#[cfg(feature = "server")]
use fabro_config::user::ExecutionMode;
use fabro_git_storage::branchstore::BranchStore;
use fabro_git_storage::gitobj::Store as GitStore;
use fabro_store::{NodeVisitRef, RuntimeState, SlateStore, Store as _};
use fabro_types::{Checkpoint, Graph, RunId, RunRecord, StartRecord, fixtures};
use git2::{Repository, Signature};
use object_store::local::LocalFileSystem;
use predicates::prelude::*;
use tokio::runtime::Runtime;

#[allow(deprecated)]
fn arc() -> Command {
    let mut cmd = Command::cargo_bin("fabro").unwrap();
    cmd.arg("--no-upgrade-check");
    cmd
}

fn parse_config_show(stdout: &[u8]) -> FabroSettings {
    serde_yaml::from_slice(stdout).expect("stdout should be valid YAML FabroSettings")
}

fn setup_config_show_fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();

    let home_fabro = home.path().join(".fabro");
    std::fs::create_dir_all(&home_fabro).unwrap();
    std::fs::write(
        home_fabro.join("user.toml"),
        r#"
verbose = true

[llm]
model = "cli-model"
provider = "openai"

[vars]
cli_only = "1"
shared = "cli"

[checkpoint]
exclude_globs = ["cli-only", "shared"]

[[hooks]]
name = "shared"
event = "run_start"
command = "echo cli"

[mcp_servers.shared]
type = "stdio"
command = ["echo", "cli"]

[sandbox]
provider = "daytona"

[sandbox.daytona]
labels = { cli_only = "1", shared = "cli" }

[sandbox.env]
CLI_ONLY = "1"
SHARED = "cli"
"#,
    )
    .unwrap();

    std::fs::write(
        project.path().join("fabro.toml"),
        r#"
version = 1

[fabro]
root = "fabro"

[llm]
model = "project-model"

[vars]
project_only = "1"
shared = "project"

[[hooks]]
name = "project"
event = "run_complete"
command = "echo project"
"#,
    )
    .unwrap();

    let workflow_dir = project.path().join("fabro").join("workflows").join("demo");
    std::fs::create_dir_all(&workflow_dir).unwrap();
    std::fs::write(
        workflow_dir.join("workflow.toml"),
        r#"
version = 1
goal = "demo goal"

[llm]
model = "run-model"
provider = "anthropic"

[vars]
run_only = "1"
shared = "run"

[checkpoint]
exclude_globs = ["run-only", "shared"]

[[hooks]]
name = "shared"
event = "run_start"
command = "echo run"

[[hooks]]
name = "run-only"
event = "run_complete"
command = "echo run-only"

[mcp_servers.shared]
type = "stdio"
command = ["echo", "run"]

[mcp_servers.run_only]
type = "stdio"
command = ["echo", "run-only"]

[sandbox.daytona]
labels = { run_only = "1", shared = "run" }

[sandbox.env]
RUN_ONLY = "1"
SHARED = "run"
"#,
    )
    .unwrap();

    std::fs::write(
        project.path().join("standalone.fabro"),
        "digraph Test { start -> end }",
    )
    .unwrap();

    (home, project)
}

fn setup_external_workflow_fixture() -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let storage_dir = home.path().join("fabro-data");

    let home_fabro = home.path().join(".fabro");
    std::fs::create_dir_all(&home_fabro).unwrap();
    std::fs::write(
        home_fabro.join("user.toml"),
        format!(
            r#"
storage_dir = "{}"
auto_approve = true

[setup]
commands = ["cli-setup"]
"#,
            storage_dir.display()
        ),
    )
    .unwrap();

    std::fs::write(
        project.path().join("fabro.toml"),
        r#"
version = 1

[setup]
commands = ["project-setup"]

[sandbox]
preserve = true
"#,
    )
    .unwrap();

    std::fs::write(
        project.path().join("workflow.fabro"),
        r#"
digraph Test {
  start [shape=Mdiamond, label="Start"]
  exit [shape=Msquare, label="Exit"]
  start -> exit
}
"#,
    )
    .unwrap();

    std::fs::write(
        project.path().join("workflow.toml"),
        r#"
version = 1
goal = "Ship it"
graph = "workflow.fabro"

[llm]
model = "claude-sonnet-4-6"

[setup]
commands = ["workflow-setup"]
"#,
    )
    .unwrap();

    (home, project, storage_dir)
}

fn init_cli_home(storage_dir: &Path) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let home_fabro = home.path().join(".fabro");
    std::fs::create_dir_all(&home_fabro).unwrap();
    let storage_dir = serde_json::to_string(&storage_dir.to_string_lossy().into_owned()).unwrap();
    std::fs::write(
        home_fabro.join("user.toml"),
        format!("storage_dir = {storage_dir}\n"),
    )
    .unwrap();
    home
}

fn list_metadata_run_ids(repo_dir: &Path) -> BTreeSet<String> {
    let repo = Repository::discover(repo_dir).unwrap();
    repo.references()
        .unwrap()
        .flatten()
        .filter_map(|reference| reference.name().map(ToOwned::to_owned))
        .filter_map(|name| {
            name.strip_prefix("refs/heads/fabro/meta/")
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn seed_run_branch(repo_dir: &Path, run_id: &str, nodes: &[&str]) -> Vec<String> {
    let repo = Repository::discover(repo_dir).unwrap();
    let store = GitStore::new(repo);
    let sig = Signature::now("Fabro", "noreply@fabro.sh").unwrap();
    let run_branch = format!("fabro/run/{run_id}");
    let empty_tree = store.write_empty_tree().unwrap();
    let mut shas = Vec::new();
    let mut parent = None;

    for node in nodes {
        let parents = parent.into_iter().collect::<Vec<_>>();
        let oid = store
            .write_commit(
                empty_tree,
                &parents,
                &format!("fabro({run_id}): {node} (completed)"),
                &sig,
            )
            .unwrap();
        store.update_ref(&run_branch, oid).unwrap();
        shas.push(oid.to_string());
        parent = Some(oid);
    }

    shas
}

fn checkpoint_record(
    current_node: &str,
    completed_nodes: &[&str],
    node_visits: &[(&str, usize)],
    git_commit_sha: Option<&str>,
) -> Checkpoint {
    Checkpoint {
        timestamp: chrono::Utc
            .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
            .single()
            .unwrap(),
        current_node: current_node.to_string(),
        completed_nodes: completed_nodes
            .iter()
            .map(|node| (*node).to_string())
            .collect(),
        node_retries: HashMap::new(),
        context_values: HashMap::new(),
        node_outcomes: HashMap::new(),
        next_node_id: None,
        git_commit_sha: git_commit_sha.map(ToOwned::to_owned),
        loop_failure_signatures: HashMap::new(),
        restart_failure_signatures: HashMap::new(),
        node_visits: node_visits
            .iter()
            .map(|(node, visit)| ((*node).to_string(), *visit))
            .collect(),
    }
}

async fn seed_durable_run(storage_dir: &Path, repo_dir: &Path, run_id: RunId) {
    let store_path = storage_dir.join("store");
    std::fs::create_dir_all(&store_path).unwrap();
    let object_store = Arc::new(LocalFileSystem::new_with_prefix(&store_path).unwrap());
    let store = SlateStore::new(object_store, "", Duration::from_millis(5));
    let created_at = chrono::Utc
        .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
        .single()
        .unwrap();
    let run_store = store.create_run(&run_id, created_at, None).await.unwrap();

    let run_record = RunRecord {
        run_id,
        created_at,
        settings: FabroSettings::default(),
        graph: Graph::default(),
        workflow_slug: None,
        working_directory: repo_dir.to_path_buf(),
        host_repo_path: Some(repo_dir.to_string_lossy().into_owned()),
        base_branch: None,
        labels: HashMap::new(),
    };
    run_store.put_run(&run_record).await.unwrap();
    run_store
        .put_start(&StartRecord {
            run_id,
            start_time: created_at,
            run_branch: Some(format!("fabro/run/{run_id}")),
            base_sha: None,
        })
        .await
        .unwrap();

    let start = NodeVisitRef {
        node_id: "start",
        visit: 1,
    };
    run_store
        .put_node_prompt(&start, "start prompt")
        .await
        .unwrap();
    let build = NodeVisitRef {
        node_id: "build",
        visit: 1,
    };
    run_store
        .put_node_prompt(&build, "build prompt")
        .await
        .unwrap();

    run_store
        .append_checkpoint(&checkpoint_record(
            "start",
            &["start"],
            &[("start", 1)],
            None,
        ))
        .await
        .unwrap();
    run_store
        .append_checkpoint(&checkpoint_record(
            "build",
            &["start", "build"],
            &[("start", 1), ("build", 1)],
            None,
        ))
        .await
        .unwrap();
}

fn metadata_checkpoints(repo_dir: &Path, run_id: &str) -> Vec<Checkpoint> {
    let repo = Repository::discover(repo_dir).unwrap();
    let store = GitStore::new(repo);
    let sig = Signature::now("Fabro", "noreply@fabro.sh").unwrap();
    let branch = format!("fabro/meta/{run_id}");
    let bs = BranchStore::new(&store, &branch, &sig);

    bs.log(100)
        .unwrap()
        .iter()
        .rev()
        .filter(|commit| commit.message.starts_with("checkpoint"))
        .map(|commit| {
            serde_json::from_slice::<Checkpoint>(
                &store
                    .read_blob_at(commit.oid, "checkpoint.json")
                    .unwrap()
                    .unwrap(),
            )
            .unwrap()
        })
        .collect()
}

fn latest_metadata_checkpoint(repo_dir: &Path, run_id: &str) -> Checkpoint {
    let repo = Repository::discover(repo_dir).unwrap();
    let store = GitStore::new(repo);
    let tip = store
        .resolve_ref(&format!("fabro/meta/{run_id}"))
        .unwrap()
        .unwrap();
    serde_json::from_slice(&store.read_blob_at(tip, "checkpoint.json").unwrap().unwrap()).unwrap()
}

// == LLM: prompt ==============================================================

#[test]
fn prompt_reads_from_stdin() {
    let result = arc()
        .args(["llm", "prompt", "--no-stream", "-m", "test-model"])
        .write_stdin("hello from stdin")
        .assert()
        .failure();

    // Should NOT complain about missing prompt
    result.stderr(predicate::str::contains("no prompt provided").not());
}

#[test]
fn prompt_concatenates_stdin_and_arg() {
    let result = arc()
        .args([
            "llm",
            "prompt",
            "--no-stream",
            "-m",
            "test-model",
            "summarize this",
        ])
        .write_stdin("some input text")
        .assert()
        .failure();

    result.stderr(predicate::str::contains("no prompt provided").not());
}

#[test]
#[ignore = "requires API key"]
fn prompt_no_stream_generates_response() {
    arc()
        .args([
            "llm",
            "prompt",
            "--no-stream",
            "-m",
            "claude-sonnet-4-5",
            "Say just the word 'hello'",
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
#[ignore = "requires API key"]
fn prompt_stream_generates_response() {
    arc()
        .args([
            "llm",
            "prompt",
            "-m",
            "claude-sonnet-4-5",
            "Say just the word 'hello'",
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
#[ignore = "requires API key"]
fn prompt_usage_shows_tokens() {
    arc()
        .args([
            "llm",
            "prompt",
            "--no-stream",
            "-u",
            "-m",
            "claude-sonnet-4-5",
            "Say just the word 'hello'",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("Tokens:"));
}

#[test]
#[ignore = "requires API key"]
fn prompt_schema_no_stream_generates_json() {
    let assert = arc()
        .args([
            "llm", "prompt", "--no-stream", "-m", "claude-sonnet-4-5",
            "--schema", r#"{"type":"object","properties":{"greeting":{"type":"string"}},"required":["greeting"]}"#,
            "Return a JSON object with a greeting field set to hello",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout should be valid JSON");
    assert!(
        parsed.get("greeting").is_some(),
        "expected 'greeting' key in output"
    );
}

#[test]
#[ignore = "requires API key"]
fn prompt_schema_stream_generates_json() {
    let assert = arc()
        .args([
            "llm", "prompt", "-m", "claude-sonnet-4-5",
            "--schema", r#"{"type":"object","properties":{"greeting":{"type":"string"}},"required":["greeting"]}"#,
            "Return a JSON object with a greeting field set to hello",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout should be valid JSON");
    assert!(
        parsed.get("greeting").is_some(),
        "expected 'greeting' key in output"
    );
}

// == LLM: chat ================================================================

#[test]
#[ignore = "requires API key"]
fn chat_multi_turn_with_system_prompt() {
    let assert = arc()
        .args([
            "llm",
            "chat",
            "-m",
            "claude-haiku-4-5",
            "-s",
            "You are a pilot. End every response with 'Roger that.'",
        ])
        .write_stdin("What is your profession?\nWhat did I just ask you?\n")
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();

    // Verify model info printed to stderr
    assert!(
        stderr.contains("Using model:"),
        "stderr should show model info"
    );

    // Verify the system prompt influenced the output
    assert!(
        stdout.to_lowercase().contains("roger that"),
        "response should follow pilot system prompt, got: {stdout}"
    );

    // Verify multi-turn: the second response should reference the first question
    assert!(
        stdout.to_lowercase().contains("profession")
            || stdout.to_lowercase().contains("asked")
            || stdout.to_lowercase().contains("pilot"),
        "second response should show multi-turn context, got: {stdout}"
    );
}

// == Exec =====================================================================

#[test]
fn exec_missing_api_key_exits_with_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    arc()
        .args(["exec", "test prompt"])
        .env_clear()
        .env("HOME", tmp.path().to_str().unwrap())
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("API key not set"));
}

#[test]
#[ignore = "requires API key"]
fn exec_creates_file() {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    arc()
        .args([
            "exec",
            "--auto-approve",
            "--permissions",
            "full",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "Create a file called hello.txt containing exactly 'Hello'",
        ])
        .current_dir(tmp.path())
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();
    let path = tmp.path().join("hello.txt");
    assert!(path.exists(), "hello.txt should have been created");
    let content = std::fs::read_to_string(&path).expect("read hello.txt");
    assert!(
        content.contains("Hello"),
        "Expected 'Hello' in hello.txt, got: {content}"
    );
}

#[test]
#[ignore = "requires API key"]
fn exec_shell_command() {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    arc()
        .args([
            "exec",
            "--auto-approve",
            "--permissions",
            "full",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "Run the shell command `echo arc_test_marker_42` and tell me what it printed",
        ])
        .current_dir(tmp.path())
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();
}

#[test]
#[ignore = "requires API key"]
fn exec_read_only_blocks_write() {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    arc()
        .args([
            "exec",
            "--auto-approve",
            "--permissions",
            "read-only",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "Create a file called forbidden.txt containing 'should not exist'",
        ])
        .current_dir(tmp.path())
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();
    assert!(
        !tmp.path().join("forbidden.txt").exists(),
        "forbidden.txt should NOT exist under read-only permissions"
    );
}

#[test]
#[ignore = "requires API key"]
fn exec_json_output_format() {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = arc()
        .args([
            "exec",
            "--auto-approve",
            "--permissions",
            "full",
            "--output-format",
            "json",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "Create a file called test.txt containing 'test'",
        ])
        .current_dir(tmp.path())
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).expect("valid utf8");
    assert!(!stdout.trim().is_empty(), "json output should not be empty");
    // Every non-empty line should be valid JSON
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(!lines.is_empty(), "should have at least one NDJSON line");
    let first: serde_json::Value =
        serde_json::from_str(lines[0]).expect("first line should be valid JSON");
    assert!(
        first.get("event").is_some() || first.get("type").is_some(),
        "NDJSON line should have an event or type field, got: {first}"
    );
}

#[test]
#[ignore = "requires API key"]
fn exec_read_and_edit() {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("data.txt"), "old content").expect("write data.txt");
    arc()
        .args([
            "exec",
            "--auto-approve",
            "--permissions",
            "full",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "Read data.txt then replace its entire content with 'new content'",
        ])
        .current_dir(tmp.path())
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success();
    let content = std::fs::read_to_string(tmp.path().join("data.txt")).expect("read data.txt");
    assert!(
        content.contains("new content"),
        "Expected 'new content' in data.txt, got: {content}"
    );
}

// == Arc: serve =========================================================

#[test]
#[cfg(feature = "server")]
fn serve_help() {
    let output = arc().args(["serve", "--help"]).output().expect("runs");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    insta::assert_snapshot!(stdout);
}

// == Doctor ===================================================================

#[test]
fn doctor_no_color_when_no_color_set() {
    arc()
        .args(["doctor", "--dry-run"])
        .env_clear()
        .env("NO_COLOR", "1")
        .assert()
        .stdout(predicate::str::contains("\x1b[").not());
}

// == JSONL logging ============================================================

#[test]
fn dry_run_writes_jsonl_and_live_json() {
    let tmp = tempfile::tempdir().unwrap();
    let storage_dir = tmp.path().join("fabro-data");

    arc()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--storage-dir",
            storage_dir.to_str().unwrap(),
            "../../../test/simple.fabro",
        ])
        .assert()
        .success();

    // Find the single run directory under storage_dir/runs/
    let runs_base = storage_dir.join("runs");
    assert!(runs_base.exists(), "runs/ directory should exist");
    let entries: Vec<_> = std::fs::read_dir(&runs_base)
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(entries.len(), 1, "should have exactly one run directory");
    let run_dir = entries[0].path();

    // progress.jsonl must exist and contain valid JSON lines
    let jsonl_path = run_dir.join("progress.jsonl");
    assert!(jsonl_path.exists(), "progress.jsonl should exist");
    let jsonl_content = std::fs::read_to_string(&jsonl_path).unwrap();
    let lines: Vec<&str> = jsonl_content.lines().collect();
    assert!(
        !lines.is_empty(),
        "progress.jsonl should have at least one line"
    );

    // Every line must be valid JSON with ts, run_id, and event keys
    let first_line: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert!(first_line.get("ts").is_some(), "line should have ts");
    assert!(
        first_line.get("run_id").is_some(),
        "line should have run_id"
    );
    assert!(first_line.get("event").is_some(), "line should have event");

    // Events should contain WorkflowRunStarted (may not be first due to exec env events)
    let has_run_started = lines.iter().any(|line| {
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        parsed["event"].as_str() == Some("WorkflowRunStarted")
    });
    assert!(has_run_started, "events should contain WorkflowRunStarted");

    // run_id should be non-empty after WorkflowRunStarted
    let last_line: serde_json::Value = serde_json::from_str(lines[lines.len() - 1]).unwrap();
    let run_id = last_line["run_id"].as_str().unwrap();
    assert!(!run_id.is_empty(), "run_id should be non-empty");

    // live.json must exist and contain valid JSON matching the last JSONL line
    let live_path = run_dir.join("live.json");
    assert!(live_path.exists(), "live.json should exist");
    let live_content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&live_path).unwrap()).unwrap();
    assert!(live_content.get("ts").is_some());
    assert!(live_content.get("run_id").is_some());
    assert!(live_content.get("event").is_some());
}

// == --run-id passthrough =====================================================

#[test]
fn run_id_passthrough_uses_provided_ulid() {
    let tmp = tempfile::tempdir().unwrap();
    let storage_dir = tmp.path().join("fabro-data");
    let my_ulid = fixtures::RUN_10.to_string();

    arc()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            my_ulid.as_str(),
            "--storage-dir",
            storage_dir.to_str().unwrap(),
            "../../../test/simple.fabro",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(&my_ulid));
}

// == --detach flag =============================================================

#[test]
fn detach_flag_appears_in_help() {
    arc()
        .args(["run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--detach"));
}

#[test]
fn detach_prints_ulid_and_exits() {
    let output = arc()
        .args([
            "run",
            "--detach",
            "--dry-run",
            "--auto-approve",
            "../../../test/simple.fabro",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let ulid = stdout.trim();
    // ULID is 26 uppercase alphanumeric chars
    assert_eq!(ulid.len(), 26, "expected 26-char ULID, got: {ulid:?}");
    assert!(
        ulid.chars().all(|c| c.is_ascii_alphanumeric()),
        "expected alphanumeric ULID, got: {ulid:?}"
    );
}

#[test]
fn detach_creates_run_dir_with_detach_log() {
    let tmp = tempfile::tempdir().unwrap();
    let storage_dir = tmp.path().join("fabro-data");

    let output = arc()
        .args([
            "run",
            "--detach",
            "--dry-run",
            "--auto-approve",
            "--storage-dir",
            storage_dir.to_str().unwrap(),
            "../../../test/simple.fabro",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let ulid = String::from_utf8(output).unwrap();
    let ulid = ulid.trim();
    assert!(!ulid.is_empty(), "should print a ULID");

    // Run dir should have been created under storage_dir/runs/ and the launcher
    // log should live under storage_dir/launchers/.
    let runs_base = storage_dir.join("runs");
    assert!(runs_base.exists(), "runs/ directory should exist");
    let entries: Vec<_> = std::fs::read_dir(&runs_base)
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(entries.len(), 1, "should have exactly one run directory");
    let run_dir = entries[0].path();
    assert!(
        storage_dir
            .join("launchers")
            .join(format!("{ulid}.log"))
            .exists(),
        "launcher log should exist under storage_dir/launchers"
    );
    assert!(!run_dir.join("detach.log").exists());
}

// == Resume ===================================================================

#[test]
fn resume_help_shows_expected_args() {
    arc()
        .args(["resume", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--detach"))
        .stdout(predicate::str::contains("--checkpoint").not())
        .stdout(predicate::str::contains("--workflow").not());
}

#[test]
fn resume_requires_run_arg() {
    arc().args(["resume"]).assert().failure();
}

#[test]
fn run_help_no_longer_shows_resume_or_run_branch() {
    arc()
        .args(["run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--resume").not())
        .stdout(predicate::str::contains("--run-branch").not());
}

#[test]
fn rewind_and_fork_recover_missing_metadata_from_store() {
    let storage_root = tempfile::tempdir().unwrap();
    let storage_dir = storage_root.path().join("fabro-data");
    let configured_home = init_cli_home(&storage_dir);
    let repo_dir = tempfile::tempdir().unwrap();
    Repository::init(repo_dir.path()).unwrap();

    let source_run_id = fixtures::RUN_1;
    let source_run_id_string = source_run_id.to_string();
    let expected_shas =
        seed_run_branch(repo_dir.path(), &source_run_id_string, &["start", "build"]);
    Runtime::new().unwrap().block_on(seed_durable_run(
        &storage_dir,
        repo_dir.path(),
        source_run_id,
    ));

    assert!(
        list_metadata_run_ids(repo_dir.path()).is_empty(),
        "metadata branch should start missing"
    );

    let rewind_list = arc()
        .env("HOME", configured_home.path())
        .env("NO_COLOR", "1")
        .current_dir(repo_dir.path())
        .args(["rewind", &source_run_id_string, "--list"])
        .timeout(Duration::from_secs(15))
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();
    let rewind_list = String::from_utf8(rewind_list).unwrap();
    assert!(
        rewind_list.contains("@1"),
        "expected first checkpoint: {rewind_list}"
    );
    assert!(
        rewind_list.contains("@2"),
        "expected second checkpoint: {rewind_list}"
    );
    assert!(
        !rewind_list.contains("no run commit"),
        "rebuilt timeline should persist backfilled SHAs: {rewind_list}"
    );

    let rebuilt_checkpoints = metadata_checkpoints(repo_dir.path(), &source_run_id_string);
    assert_eq!(rebuilt_checkpoints.len(), 2);
    assert_eq!(
        rebuilt_checkpoints[0].git_commit_sha.as_deref(),
        Some(expected_shas[0].as_str())
    );
    assert_eq!(
        rebuilt_checkpoints[1].git_commit_sha.as_deref(),
        Some(expected_shas[1].as_str())
    );

    let before_child = list_metadata_run_ids(repo_dir.path());
    arc()
        .env("HOME", configured_home.path())
        .env("NO_COLOR", "1")
        .current_dir(repo_dir.path())
        .args(["fork", &source_run_id_string, "--no-push"])
        .timeout(Duration::from_secs(15))
        .assert()
        .success();
    let after_child = list_metadata_run_ids(repo_dir.path());
    let child_run_ids: Vec<_> = after_child.difference(&before_child).cloned().collect();
    assert_eq!(child_run_ids.len(), 1, "expected one child run");
    let child_run_id = &child_run_ids[0];

    let child_checkpoint = latest_metadata_checkpoint(repo_dir.path(), child_run_id);
    assert_eq!(
        child_checkpoint.git_commit_sha.as_deref(),
        Some(expected_shas[1].as_str())
    );

    let child_rewind = arc()
        .env("HOME", configured_home.path())
        .env("NO_COLOR", "1")
        .current_dir(repo_dir.path())
        .args(["rewind", child_run_id, "@1", "--no-push"])
        .timeout(Duration::from_secs(15))
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();
    let child_rewind = String::from_utf8(child_rewind).unwrap();
    assert!(
        child_rewind.contains("Rewound run branch"),
        "expected child rewind to move the run branch: {child_rewind}"
    );
    assert!(
        !child_rewind.contains("has no git_commit_sha"),
        "child rewind should not lose git_commit_sha: {child_rewind}"
    );

    let before_grandchild = after_child;
    arc()
        .env("HOME", configured_home.path())
        .env("NO_COLOR", "1")
        .current_dir(repo_dir.path())
        .args(["fork", child_run_id, "--no-push"])
        .timeout(Duration::from_secs(15))
        .assert()
        .success();
    let after_grandchild = list_metadata_run_ids(repo_dir.path());
    let grandchild_run_ids: Vec<_> = after_grandchild
        .difference(&before_grandchild)
        .cloned()
        .collect();
    assert_eq!(grandchild_run_ids.len(), 1, "expected one grandchild run");
}

// == Bug regression: create/start/attach lifecycle ============================

/// Helper: create a minimal run directory that `resolve_run` can find.
/// Sets up run.json, status.json, and progress.jsonl.
fn setup_run_dir(
    home: &std::path::Path,
    run_id: &str,
    spec_overrides: serde_json::Value,
    progress_lines: &[&str],
) -> std::path::PathBuf {
    let run_dir = home.join(".fabro").join("runs").join(run_id);
    std::fs::create_dir_all(&run_dir).unwrap();

    // Build defaults, then merge overrides
    let overrides = spec_overrides;
    let get_str = |key: &str, default: &str| -> serde_json::Value {
        overrides
            .get(key)
            .and_then(|v| v.as_str())
            .map_or_else(|| serde_json::json!(default), |s| serde_json::json!(s))
    };
    let get_bool = |key: &str, default: bool| -> serde_json::Value {
        overrides
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .map_or_else(|| serde_json::json!(default), |b| serde_json::json!(b))
    };

    // run.json (RunRecord) for resolve_run and run_engine_entrypoint
    let run_record = serde_json::json!({
        "run_id": run_id,
        "created_at": "2026-01-01T00:00:00Z",
        "settings": {
            "goal": overrides.get("goal").and_then(|v| v.as_str()),
            "llm": {
                "model": get_str("model", "test-model"),
                "provider": overrides.get("provider").and_then(|v| v.as_str())
            },
            "sandbox": {
                "provider": get_str("sandbox_provider", "local"),
                "preserve": get_bool("preserve_sandbox", false)
            },
            "verbose": get_bool("verbose", false),
            "dry_run": get_bool("dry_run", true),
            "auto_approve": get_bool("auto_approve", true),
            "no_retro": get_bool("no_retro", true)
        },
        "graph": {
            "name": "test",
            "nodes": {},
            "edges": [],
            "attrs": {}
        },
        "working_directory": overrides.get("working_directory").and_then(|v| v.as_str()).unwrap_or("/tmp"),
        "labels": overrides.get("labels").cloned().unwrap_or(serde_json::json!({}))
    });
    std::fs::write(
        run_dir.join("run.json"),
        serde_json::to_string(&run_record).unwrap(),
    )
    .unwrap();

    // progress.jsonl
    std::fs::write(run_dir.join("progress.jsonl"), progress_lines.join("\n")).unwrap();

    run_dir
}

fn find_run_dir(home: &std::path::Path, run_id: &str) -> std::path::PathBuf {
    let runs_dir = home.join(".fabro").join("runs");
    std::fs::read_dir(&runs_dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(run_id))
        })
        .unwrap_or_else(|| {
            panic!(
                "expected run directory for {run_id} under {}",
                runs_dir.display()
            )
        })
}

#[test]
fn completed_run_preserves_workflow_slug_for_lookup() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let workflow_dir = project.path().join("workflows").join("sluggy");
    let run_id = fixtures::RUN_11.to_string();
    std::fs::create_dir_all(&workflow_dir).unwrap();
    let workflow_path = workflow_dir.join("workflow.fabro");
    std::fs::write(
        &workflow_path,
        "\
digraph BarBaz {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    )
    .unwrap();

    arc()
        .env("HOME", home.path())
        .current_dir(project.path())
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    arc()
        .env("HOME", home.path())
        .current_dir(project.path())
        .args(["start", "sluggy"])
        .assert()
        .success();

    arc()
        .env("HOME", home.path())
        .current_dir(project.path())
        .args(["attach", run_id.as_str()])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    arc()
        .env("HOME", home.path())
        .current_dir(project.path())
        .args(["attach", "sluggy"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let run_dir = find_run_dir(home.path(), &run_id);
    let run_record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(run_record["graph"]["name"].as_str(), Some("BarBaz"));
    assert_eq!(run_record["workflow_slug"].as_str(), Some("sluggy"));
}

#[test]
fn standalone_file_run_uses_file_stem_slug_for_lookup() {
    let home = tempfile::tempdir().unwrap();
    let workflow_dir = tempfile::tempdir().unwrap();
    let workflow_path = workflow_dir.path().join("alpha.fabro");
    let run_id = fixtures::RUN_12.to_string();
    std::fs::write(
        &workflow_path,
        "\
digraph FooWorkflow {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    )
    .unwrap();

    arc()
        .env("HOME", home.path())
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    arc()
        .env("HOME", home.path())
        .args(["start", "alpha"])
        .assert()
        .success();

    arc()
        .env("HOME", home.path())
        .args(["attach", "alpha"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let run_dir = find_run_dir(home.path(), &run_id);
    let run_record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(run_record["graph"]["name"].as_str(), Some("FooWorkflow"));
    assert_eq!(run_record["workflow_slug"].as_str(), Some("alpha"));
}

#[test]
fn dry_run_create_start_attach_works_with_default_run_lookup() {
    let home = tempfile::tempdir().unwrap();
    let run_id = fixtures::RUN_13.to_string();

    arc()
        .env("HOME", home.path())
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            "../../../test/simple.fabro",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(&run_id));

    let run_dir = find_run_dir(home.path(), &run_id);
    assert!(
        run_dir.join("run.json").exists(),
        "create should persist run.json so the run is discoverable"
    );

    arc()
        .env("HOME", home.path())
        .args(["start", run_id.as_str()])
        .assert()
        .success();

    arc()
        .env("HOME", home.path())
        .args(["attach", run_id.as_str()])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    assert!(run_dir.join("conclusion.json").exists());
}

#[test]
fn dry_run_detach_attach_works_with_default_run_lookup() {
    let home = tempfile::tempdir().unwrap();
    let run_id = fixtures::RUN_14.to_string();

    arc()
        .env("HOME", home.path())
        .args([
            "run",
            "--detach",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            "../../../test/simple.fabro",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(&run_id));

    arc()
        .env("HOME", home.path())
        .args(["attach", run_id.as_str()])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();
}

#[test]
fn start_by_workflow_name_prefers_newly_created_submitted_run() {
    let home = tempfile::tempdir().unwrap();
    let old_run_dir = home.path().join(".fabro").join("runs").join("old-smoke");
    let old_run_id = fixtures::RUN_15.to_string();
    let run_id = fixtures::RUN_16.to_string();
    std::fs::create_dir_all(&old_run_dir).unwrap();
    std::fs::write(
        old_run_dir.join("run.json"),
        serde_json::json!({
            "run_id": old_run_id,
            "created_at": "2026-01-01T00:00:00Z",
            "settings": {},
            "graph": {
                "name": "Smoke",
                "nodes": {},
                "edges": [],
                "attrs": {}
            },
            "working_directory": "/tmp"
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        old_run_dir.join("status.json"),
        serde_json::json!({"status": "succeeded", "updated_at": "2026-01-01T00:00:00Z"})
            .to_string(),
    )
    .unwrap();

    arc()
        .env("HOME", home.path())
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id.as_str(),
            "smoke",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(&run_id));

    arc()
        .env("HOME", home.path())
        .args(["start", "smoke"])
        .assert()
        .success();

    arc()
        .env("HOME", home.path())
        .args(["attach", run_id.as_str()])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let new_run_dir = find_run_dir(home.path(), &run_id);
    let status = std::fs::read_to_string(new_run_dir.join("status.json")).unwrap();
    assert!(
        status.contains("\"status\": \"succeeded\""),
        "expected the newly created Smoke run to be started and completed"
    );
}

// Bug 2: __detached should use cached graph.fabro, not run.json working_directory.
// When the original workflow file is deleted between create and start,
// the engine should read the snapshot saved at create time.
#[test]
fn bug2_detached_uses_cached_graph_not_original_path() {
    let dir = tempfile::tempdir().unwrap();
    let storage_dir = dir.path().join("storage");
    let run_dir = storage_dir.join("runs").join("20260101-test-bug2");
    let run_id = fixtures::RUN_17.to_string();
    std::fs::create_dir_all(&run_dir).unwrap();

    let dot = "\
digraph G {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare,  label=\"Exit\"]
  start -> exit
}";

    // run.json: working_directory is valid but original workflow path no longer exists
    let run_record = serde_json::json!({
        "run_id": run_id,
        "created_at": "2026-01-01T00:00:00Z",
        "settings": {
            "dry_run": true,
            "auto_approve": true,
            "no_retro": true,
            "llm": {
                "model": "test-model"
            },
            "sandbox": {
                "provider": "local"
            }
        },
        "graph": {
            "name": "G",
            "nodes": {},
            "edges": [],
            "attrs": {}
        },
        "working_directory": run_dir.to_str().unwrap(),
    });
    std::fs::write(
        run_dir.join("run.json"),
        serde_json::to_string(&run_record).unwrap(),
    )
    .unwrap();

    // The cached graph snapshot saved by `fabro create`
    std::fs::write(run_dir.join("graph.fabro"), dot).unwrap();

    // __detached should use graph.fabro and never reference the deleted file.
    let output = arc()
        .args([
            "__detached",
            "--run-dir",
            run_dir.to_str().unwrap(),
            "--launcher-path",
            storage_dir
                .join("launchers")
                .join("test-bug2.json")
                .to_str()
                .unwrap(),
        ])
        .env("NO_COLOR", "1")
        .timeout(std::time::Duration::from_secs(15))
        .output()
        .expect("process should start");

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains("deleted-workflow.fabro"),
        "bug2: engine should use cached graph.fabro, not the original \
         (deleted) workflow path.\nstderr: {stderr}"
    );
}

#[test]
fn bug4_detached_resume_rejects_completed_run_without_mutating_it() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let workflow_path = project.path().join("workflow.fabro");
    std::fs::write(
        &workflow_path,
        "\
digraph Test {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    )
    .unwrap();

    let run = arc()
        .env("HOME", home.path())
        .current_dir(project.path())
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--no-retro",
            "--detach",
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    let run_id = String::from_utf8(run.get_output().stdout.clone())
        .unwrap()
        .trim()
        .to_string();

    arc()
        .env("HOME", home.path())
        .args(["wait", &run_id])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let inspect_before = arc()
        .env("HOME", home.path())
        .args(["inspect", &run_id])
        .assert()
        .success();
    let before: serde_json::Value =
        serde_json::from_slice(&inspect_before.get_output().stdout).unwrap();
    let run_dir = before[0]["run_dir"].as_str().unwrap().to_string();
    let start_time_before = before[0]["start_record"]["start_time"]
        .as_str()
        .unwrap()
        .to_string();
    let conclusion_ts_before = before[0]["conclusion"]["timestamp"]
        .as_str()
        .unwrap()
        .to_string();

    let storage_dir = home.path().join(".fabro");
    arc()
        .env("HOME", home.path())
        .args([
            "__detached",
            "--run-dir",
            &run_dir,
            "--launcher-path",
            storage_dir
                .join("launchers")
                .join(format!("{run_id}.json"))
                .to_str()
                .unwrap(),
            "--resume",
        ])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .failure()
        .stderr(predicate::str::contains("nothing to resume"));

    let inspect_after = arc()
        .env("HOME", home.path())
        .args(["inspect", &run_id])
        .assert()
        .success();
    let after: serde_json::Value =
        serde_json::from_slice(&inspect_after.get_output().stdout).unwrap();

    assert_eq!(
        after[0]["start_record"]["start_time"].as_str().unwrap(),
        start_time_before
    );
    assert_eq!(
        after[0]["conclusion"]["timestamp"].as_str().unwrap(),
        conclusion_ts_before
    );
}

#[test]
fn bug5_detached_uses_snapshotted_app_id_for_github_credentials() {
    let storage_dir = tempfile::tempdir().unwrap();
    let home = init_cli_home(storage_dir.path());
    let run_dir = storage_dir.path().join("runs").join("20260101-test-bug5");
    let run_id = fixtures::RUN_18.to_string();
    std::fs::create_dir_all(&run_dir).unwrap();

    let dot = "\
digraph G {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare,  label=\"Exit\"]
  start -> exit
}";

    let run_record = serde_json::json!({
        "run_id": run_id,
        "created_at": "2026-01-01T00:00:00Z",
        "settings": {
            "dry_run": true,
            "auto_approve": true,
            "no_retro": true,
            "llm": {
                "model": "test-model"
            },
            "sandbox": {
                "provider": "local"
            },
            "git": {
                "app_id": "snapshotted-app-id"
            }
        },
        "graph": {
            "name": "G",
            "nodes": {},
            "edges": [],
            "attrs": {}
        },
        "working_directory": run_dir.to_str().unwrap(),
    });
    std::fs::write(
        run_dir.join("run.json"),
        serde_json::to_string(&run_record).unwrap(),
    )
    .unwrap();
    std::fs::write(run_dir.join("graph.fabro"), dot).unwrap();

    arc()
        .env("HOME", home.path())
        .env("GITHUB_APP_PRIVATE_KEY", "%%%not-base64%%%")
        .args([
            "__detached",
            "--run-dir",
            run_dir.to_str().unwrap(),
            "--launcher-path",
            storage_dir
                .path()
                .join("launchers")
                .join("test-bug5.json")
                .to_str()
                .unwrap(),
        ])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "GITHUB_APP_PRIVATE_KEY is not valid PEM or base64",
        ));
}

// Bug 3: attach loop must leave interview_request.json in place until the
// engine consumes interview_response.json, so reattach remains safe.
#[test]
fn bug3_attach_leaves_interview_request_until_engine_consumes_response() {
    let home = tempfile::tempdir().unwrap();
    let run_id = fixtures::RUN_7.to_string();
    let stage_started = format!(
        r#"{{"ts":"2026-01-01T00:00:01Z","run_id":"{run_id}","event":"StageStarted","node_id":"gate","name":"Gate","index":0,"attempt":1,"max_attempts":1}}"#
    );

    let run_dir = setup_run_dir(
        home.path(),
        &run_id,
        serde_json::json!({}),
        &[&stage_started],
    );

    // Terminal status still allows attach to answer the interview once before exiting.
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::json!({"status": "succeeded", "updated_at": "2026-01-01T00:00:00Z"})
            .to_string(),
    )
    .unwrap();

    // interview_request.json — a question the engine wrote
    let question = serde_json::json!({
        "text": "Approve?",
        "question_type": "YesNo",
        "options": [],
        "allow_freeform": false,
        "default": {"value": "Yes", "selected_option": null, "text": null},
        "timeout_seconds": 1.0,
        "stage": "gate",
        "metadata": {}
    });
    let runtime_state = RuntimeState::new(&run_dir);
    std::fs::create_dir_all(runtime_state.runtime_dir()).unwrap();
    std::fs::write(
        runtime_state.interview_request_path(),
        serde_json::to_string(&question).unwrap(),
    )
    .unwrap();

    // Pipe "y\n" so ConsoleInterviewer doesn't block on stdin
    let _ = arc()
        .env("HOME", home.path())
        .env("NO_COLOR", "1")
        .args(["attach", &run_id])
        .write_stdin("y\n")
        .timeout(std::time::Duration::from_secs(5))
        .output();

    // The attach loop should leave the request durable until the engine consumes
    // the response, so a crashed attach can be retried safely.
    assert!(
        runtime_state.interview_request_path().exists(),
        "bug3: interview_request.json should stay present until the engine consumes the answer"
    );
    assert!(
        runtime_state.interview_response_path().exists(),
        "bug3: attach should write interview_response.json after handling the prompt"
    );
    let response = std::fs::read_to_string(runtime_state.interview_response_path()).unwrap();
    assert!(response.contains("\"value\": \"Yes\""));
}

#[test]
fn attach_closed_stdin_keeps_interview_pending() {
    let home = tempfile::tempdir().unwrap();
    let run_id = fixtures::RUN_8.to_string();
    let stage_started = format!(
        r#"{{"ts":"2026-01-01T00:00:01Z","run_id":"{run_id}","event":"StageStarted","node_id":"gate","name":"Gate","index":0,"attempt":1,"max_attempts":1}}"#
    );

    let run_dir = setup_run_dir(
        home.path(),
        &run_id,
        serde_json::json!({}),
        &[&stage_started],
    );

    std::fs::write(
        run_dir.join("status.json"),
        serde_json::json!({"status": "running", "updated_at": "2026-01-01T00:00:00Z"}).to_string(),
    )
    .unwrap();

    let question = serde_json::json!({
        "text": "Approve?",
        "question_type": "YesNo",
        "options": [],
        "allow_freeform": false,
        "default": null,
        "timeout_seconds": null,
        "stage": "gate",
        "metadata": {}
    });
    let runtime_state = RuntimeState::new(&run_dir);
    std::fs::create_dir_all(runtime_state.runtime_dir()).unwrap();
    std::fs::write(
        runtime_state.interview_request_path(),
        serde_json::to_string(&question).unwrap(),
    )
    .unwrap();

    let assert = arc()
        .env("HOME", home.path())
        .env("NO_COLOR", "1")
        .args(["attach", &run_id])
        .timeout(std::time::Duration::from_secs(5))
        .assert()
        .failure();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("still waiting for input"),
        "attach should explain that the run is still waiting for a human answer.\nstderr: {stderr}"
    );
    assert!(
        runtime_state.interview_request_path().exists(),
        "attach with closed stdin must leave the request pending"
    );
    assert!(
        !runtime_state.interview_response_path().exists(),
        "attach with closed stdin must not fabricate a response"
    );
    assert!(
        !runtime_state.interview_claim_path().exists(),
        "attach with closed stdin must release the claim so a later attach can answer"
    );
}

// Bug 4: attach should respect the verbose flag from run.json.
// Currently ProgressUI is created with verbose=false regardless of config.
#[test]
fn bug4_attach_respects_verbose_from_spec() {
    let home = tempfile::tempdir().unwrap();
    let run_id = fixtures::RUN_9.to_string();

    // Use pre-rename field names so handle_json_line can parse them
    // (isolates this test from bug 1). With 2 turns and 1 tool call,
    // verbose mode should display "(2 turns, 1 tools, …)" in the output.
    let run_dir = setup_run_dir(
        home.path(),
        &run_id,
        serde_json::json!({"verbose": true}),
        &[
            &format!(
                r#"{{"ts":"2026-01-01T12:00:00Z","run_id":"{run_id}","event":"StageStarted","node_id":"code","name":"Code","index":0,"attempt":1,"max_attempts":1}}"#
            ),
            &format!(
                r#"{{"ts":"2026-01-01T12:00:01Z","run_id":"{run_id}","event":"Agent.AssistantMessage","stage":"code","model":"claude-sonnet"}}"#
            ),
            &format!(
                r#"{{"ts":"2026-01-01T12:00:02Z","run_id":"{run_id}","event":"Agent.AssistantMessage","stage":"code","model":"claude-sonnet"}}"#
            ),
            &format!(
                r#"{{"ts":"2026-01-01T12:00:03Z","run_id":"{run_id}","event":"Agent.ToolCallStarted","stage":"code","tool_name":"read_file","tool_call_id":"tc1","arguments":{{}}}}"#
            ),
            &format!(
                r#"{{"ts":"2026-01-01T12:00:04Z","run_id":"{run_id}","event":"Agent.ToolCallCompleted","stage":"code","tool_name":"read_file","tool_call_id":"tc1","is_error":false}}"#
            ),
            &format!(
                r#"{{"ts":"2026-01-01T12:00:10Z","run_id":"{run_id}","event":"StageCompleted","node_id":"code","name":"Code","index":0,"duration_ms":10000,"status":"success","usage":{{"input_tokens":1000,"output_tokens":500}}}}"#
            ),
        ],
    );

    // Succeeded status + conclusion so attach exits after reading events
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::json!({"status": "succeeded", "updated_at": "2026-01-01T12:00:10Z"})
            .to_string(),
    )
    .unwrap();
    std::fs::write(
        run_dir.join("conclusion.json"),
        serde_json::json!({
            "timestamp": "2026-01-01T12:00:10Z",
            "status": "success",
            "duration_ms": 10000,
            "stages": [],
            "total_retries": 0
        })
        .to_string(),
    )
    .unwrap();

    let output = arc()
        .env("HOME", home.path())
        .env("NO_COLOR", "1")
        .args(["attach", &run_id])
        .timeout(std::time::Duration::from_secs(10))
        .output()
        .expect("process should start");

    let stderr = String::from_utf8(output.stderr).unwrap();

    // Bug: verbose is hardcoded false, so stats are suppressed.
    // Fix: load spec.verbose and pass it to ProgressUI.
    assert!(
        stderr.contains("turns") && stderr.contains("tools"),
        "bug4: attach should show verbose stats when spec.verbose=true.\nstderr: {stderr}"
    );
}

#[test]
fn config_show_merges_cli_and_project_defaults() {
    let (home, project) = setup_config_show_fixture();

    let output = arc()
        .env("HOME", home.path())
        .current_dir(project.path())
        .args(["config", "show"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let cfg = parse_config_show(&output);
    let llm = cfg.llm.as_ref().expect("llm config");
    assert_eq!(llm.model.as_deref(), Some("project-model"));
    assert_eq!(llm.provider.as_deref(), Some("openai"));
    assert_eq!(cfg.goal.as_deref(), None);
    assert_eq!(cfg.fabro.as_ref().map(|f| f.root.as_str()), Some("fabro"));

    let vars = cfg.vars.as_ref().expect("vars");
    assert_eq!(vars.get("cli_only").map(String::as_str), Some("1"));
    assert_eq!(vars.get("project_only").map(String::as_str), Some("1"));
    assert_eq!(vars.get("shared").map(String::as_str), Some("project"));

    let sandbox = cfg.sandbox.as_ref().expect("sandbox");
    let labels = sandbox
        .daytona
        .as_ref()
        .and_then(|d| d.labels.as_ref())
        .expect("daytona labels");
    assert_eq!(labels.get("cli_only").map(String::as_str), Some("1"));
    assert_eq!(labels.get("shared").map(String::as_str), Some("cli"));
}

#[test]
fn config_show_workflow_name_applies_run_overlay_and_deep_merges() {
    let (home, project) = setup_config_show_fixture();

    let output = arc()
        .env("HOME", home.path())
        .current_dir(project.path())
        .args(["config", "show", "demo"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let cfg = parse_config_show(&output);
    let llm = cfg.llm.as_ref().expect("llm config");
    assert_eq!(cfg.goal.as_deref(), Some("demo goal"));
    assert_eq!(llm.model.as_deref(), Some("run-model"));
    assert_eq!(llm.provider.as_deref(), Some("anthropic"));

    let vars = cfg.vars.as_ref().expect("vars");
    assert_eq!(vars.get("cli_only").map(String::as_str), Some("1"));
    assert_eq!(vars.get("project_only").map(String::as_str), Some("1"));
    assert_eq!(vars.get("run_only").map(String::as_str), Some("1"));
    assert_eq!(vars.get("shared").map(String::as_str), Some("run"));

    assert_eq!(
        cfg.checkpoint.exclude_globs,
        vec![
            "cli-only".to_string(),
            "run-only".to_string(),
            "shared".to_string()
        ]
    );

    assert_eq!(cfg.hooks.len(), 3);
    let shared_hook = cfg
        .hooks
        .iter()
        .find(|hook| hook.name.as_deref() == Some("shared"))
        .expect("shared hook");
    assert_eq!(shared_hook.command.as_deref(), Some("echo run"));
    assert!(
        cfg.hooks
            .iter()
            .any(|hook| hook.name.as_deref() == Some("project"))
    );
    assert!(
        cfg.hooks
            .iter()
            .any(|hook| hook.name.as_deref() == Some("run-only"))
    );

    match &cfg.mcp_servers["shared"].transport {
        McpTransport::Stdio { command, .. } => assert_eq!(command, &vec!["echo", "run"]),
        other => panic!("unexpected MCP transport: {other:?}"),
    }
    assert!(cfg.mcp_servers.contains_key("run_only"));

    let sandbox = cfg.sandbox.as_ref().expect("sandbox");
    let labels = sandbox
        .daytona
        .as_ref()
        .and_then(|d| d.labels.as_ref())
        .expect("daytona labels");
    assert_eq!(labels.get("cli_only").map(String::as_str), Some("1"));
    assert_eq!(labels.get("run_only").map(String::as_str), Some("1"));
    assert_eq!(labels.get("shared").map(String::as_str), Some("run"));

    let env = sandbox.env.as_ref().expect("sandbox env");
    assert_eq!(env.get("CLI_ONLY").map(String::as_str), Some("1"));
    assert_eq!(env.get("RUN_ONLY").map(String::as_str), Some("1"));
    assert_eq!(env.get("SHARED").map(String::as_str), Some("run"));
}

#[test]
fn config_show_explicit_workflow_path_uses_workflow_project_layers() {
    let (home, project, _storage_dir) = setup_external_workflow_fixture();
    let cwd = tempfile::tempdir().unwrap();
    let workflow = project.path().join("workflow.toml");

    let output = arc()
        .env("HOME", home.path())
        .current_dir(cwd.path())
        .args(["config", "show", workflow.to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let cfg = parse_config_show(&output);
    assert_eq!(cfg.auto_approve, Some(true));
    assert_eq!(
        cfg.setup.as_ref().expect("setup config").commands,
        vec![
            "workflow-setup".to_string(),
            "project-setup".to_string(),
            "cli-setup".to_string(),
        ]
    );
    assert_eq!(
        cfg.sandbox.as_ref().expect("sandbox config").preserve,
        Some(true)
    );
}

#[test]
fn create_explicit_workflow_path_uses_project_config_relative_to_workflow() {
    let (home, project, storage_dir) = setup_external_workflow_fixture();
    let cwd = tempfile::tempdir().unwrap();
    let workflow = project.path().join("workflow.toml");
    let run_id = fixtures::RUN_19.to_string();

    arc()
        .env("HOME", home.path())
        .current_dir(cwd.path())
        .args([
            "create",
            "--dry-run",
            "--model",
            "gpt-5.2",
            "--run-id",
            run_id.as_str(),
            workflow.to_str().unwrap(),
        ])
        .assert()
        .success();

    let runs_dir = storage_dir.join("runs");
    let run_dir = std::fs::read_dir(&runs_dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(&run_id))
        })
        .unwrap_or_else(|| {
            panic!(
                "expected run directory for {run_id} under {}",
                runs_dir.display()
            )
        });

    let run_record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(run_record["settings"]["auto_approve"].as_bool(), Some(true));
    assert_eq!(
        run_record["settings"]["storage_dir"].as_str(),
        Some(storage_dir.to_str().unwrap())
    );
    assert_eq!(
        run_record["settings"]["sandbox"]["preserve"].as_bool(),
        Some(true)
    );
    assert_eq!(
        run_record["settings"]["llm"]["model"].as_str(),
        Some("gpt-5.2")
    );
    assert_eq!(
        run_record["settings"]["setup"]["commands"],
        serde_json::json!(["workflow-setup", "project-setup", "cli-setup"])
    );
}

#[test]
fn config_show_fabro_path_matches_ambient_defaults() {
    let (home, project) = setup_config_show_fixture();

    let ambient = arc()
        .env("HOME", home.path())
        .current_dir(project.path())
        .args(["config", "show"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let graph = arc()
        .env("HOME", home.path())
        .current_dir(project.path())
        .args(["config", "show", "standalone.fabro"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(parse_config_show(&graph), parse_config_show(&ambient));
}

#[test]
fn config_show_missing_run_config_errors() {
    let (home, project) = setup_config_show_fixture();

    arc()
        .env("HOME", home.path())
        .current_dir(project.path())
        .args(["config", "show", "missing.toml"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Workflow not found"));
}

#[test]
fn config_show_legacy_cli_config_warns_and_ignores_it() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();

    let home_fabro = home.path().join(".fabro");
    std::fs::create_dir_all(&home_fabro).unwrap();
    std::fs::write(
        home_fabro.join("cli.toml"),
        r#"
verbose = true

[llm]
model = "legacy-model"
"#,
    )
    .unwrap();

    let assert = arc()
        .env("HOME", home.path())
        .current_dir(project.path())
        .args(["config", "show"])
        .assert()
        .success()
        .stderr(predicate::str::contains("ignoring legacy config file"))
        .stderr(predicate::str::contains("Rename it to"));

    let cfg = parse_config_show(&assert.get_output().stdout);
    assert_eq!(cfg.verbose, None);
    assert_eq!(cfg.llm, None);
}

#[test]
fn config_show_user_config_wins_over_legacy_cli_config() {
    let (home, project) = setup_config_show_fixture();
    std::fs::write(
        home.path().join(".fabro").join("cli.toml"),
        r#"
[llm]
model = "legacy-model"

[vars]
shared = "legacy"
"#,
    )
    .unwrap();

    let assert = arc()
        .env("HOME", home.path())
        .current_dir(project.path())
        .args(["config", "show"])
        .assert()
        .success()
        .stderr(predicate::str::contains("ignoring legacy config file"));

    let cfg = parse_config_show(&assert.get_output().stdout);
    let llm = cfg.llm.as_ref().expect("llm config");
    assert_eq!(llm.model.as_deref(), Some("project-model"));
    assert_eq!(
        cfg.vars
            .as_ref()
            .and_then(|vars| vars.get("shared").map(String::as_str)),
        Some("project")
    );
}

#[test]
#[cfg(feature = "server")]
fn config_show_server_url_overrides_cli_defaults() {
    let (home, project) = setup_config_show_fixture();
    let user_toml = home.path().join(".fabro").join("user.toml");
    std::fs::write(
        &user_toml,
        format!(
            "{}\nmode = \"standalone\"\n[server]\nbase_url = \"https://config.example.com\"\n",
            std::fs::read_to_string(&user_toml).unwrap()
        ),
    )
    .unwrap();

    let output = arc()
        .env("HOME", home.path())
        .current_dir(project.path())
        .args(["--server-url", "https://cli.example.com", "config", "show"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let cfg = parse_config_show(&output);
    assert_eq!(cfg.mode, Some(ExecutionMode::Server));
    assert_eq!(
        cfg.server
            .as_ref()
            .and_then(|server| server.base_url.as_deref()),
        Some("https://cli.example.com")
    );
}
