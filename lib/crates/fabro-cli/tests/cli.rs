use assert_cmd::Command;
use predicates::prelude::*;

#[allow(deprecated)]
fn arc() -> Command {
    Command::cargo_bin("fabro").unwrap()
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
    let run_dir = tmp.path().join("run");

    arc()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--run-dir",
            run_dir.to_str().unwrap(),
            "../../../test/simple.fabro",
        ])
        .assert()
        .success();

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
    let run_dir = tmp.path().join("run");
    let my_ulid = "01JTEST1234567890ABCDE";

    arc()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            my_ulid,
            "--run-dir",
            run_dir.to_str().unwrap(),
            "../../../test/simple.fabro",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(my_ulid));
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
    let run_dir = tmp.path().join("detached-run");

    let output = arc()
        .args([
            "run",
            "--detach",
            "--dry-run",
            "--auto-approve",
            "--run-dir",
            run_dir.to_str().unwrap(),
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

    // Run dir should have been created with detach.log
    assert!(run_dir.exists(), "run dir should exist");
    assert!(
        run_dir.join("detach.log").exists(),
        "detach.log should exist in run dir"
    );
}

// == Resume ===================================================================

#[test]
fn resume_help_shows_expected_args() {
    arc()
        .args(["resume", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--checkpoint"))
        .stdout(predicate::str::contains("--workflow"));
}

#[test]
fn resume_requires_run_or_checkpoint() {
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

// == Bug regression: create/start/attach lifecycle ============================

/// Helper: create a minimal run directory that `resolve_run` can find.
/// Sets up manifest.json, status.json, spec.json, and progress.jsonl.
fn setup_run_dir(
    home: &std::path::Path,
    run_id: &str,
    spec_overrides: serde_json::Value,
    progress_lines: &[&str],
) -> std::path::PathBuf {
    let run_dir = home.join(".fabro").join("runs").join(run_id);
    std::fs::create_dir_all(&run_dir).unwrap();

    // manifest.json for resolve_run
    let manifest = serde_json::json!({
        "run_id": run_id,
        "workflow_name": "test",
        "goal": "",
        "start_time": "2026-01-01T00:00:00Z",
        "node_count": 1,
        "edge_count": 0
    });
    std::fs::write(
        run_dir.join("manifest.json"),
        serde_json::to_string(&manifest).unwrap(),
    )
    .unwrap();

    // Merge spec defaults with overrides
    let mut spec = serde_json::json!({
        "run_id": run_id,
        "workflow_path": "/tmp/test.fabro",
        "dot_source": "digraph { start -> exit }",
        "working_directory": "/tmp",
        "goal": null,
        "model": "test-model",
        "provider": null,
        "sandbox_provider": "local",
        "labels": {},
        "verbose": false,
        "no_retro": true,

        "preserve_sandbox": false,
        "dry_run": true,
        "auto_approve": true
    });
    if let (Some(base), Some(overrides)) = (spec.as_object_mut(), spec_overrides.as_object()) {
        for (k, v) in overrides {
            base.insert(k.clone(), v.clone());
        }
    }
    std::fs::write(
        run_dir.join("spec.json"),
        serde_json::to_string(&spec).unwrap(),
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
            "opaque-run-999",
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
        .args(["attach", "opaque-run-999"])
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

    let run_dir = find_run_dir(home.path(), "opaque-run-999");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["workflow_name"].as_str(), Some("BarBaz"));
    assert_eq!(manifest["workflow_slug"].as_str(), Some("sluggy"));
}

#[test]
fn standalone_file_run_uses_file_stem_slug_for_lookup() {
    let home = tempfile::tempdir().unwrap();
    let workflow_dir = tempfile::tempdir().unwrap();
    let workflow_path = workflow_dir.path().join("alpha.fabro");
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
            "opaque-run-alpha",
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

    let run_dir = find_run_dir(home.path(), "opaque-run-alpha");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["workflow_name"].as_str(), Some("FooWorkflow"));
    assert_eq!(manifest["workflow_slug"].as_str(), Some("alpha"));
}

#[test]
fn resumed_run_preserves_workflow_slug_for_lookup() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let workflow_dir = project.path().join("workflows").join("sluggy");
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
    let original_run_dir = project.path().join("original-run");

    arc()
        .env("HOME", home.path())
        .current_dir(project.path())
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--no-retro",
            "--run-dir",
            original_run_dir.to_str().unwrap(),
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    arc()
        .env("HOME", home.path())
        .current_dir(project.path())
        .args([
            "resume",
            "--checkpoint",
            original_run_dir.join("checkpoint.json").to_str().unwrap(),
            "--workflow",
            workflow_path.to_str().unwrap(),
            "--dry-run",
            "--auto-approve",
            "--no-retro",
        ])
        .assert()
        .success();

    arc()
        .env("HOME", home.path())
        .current_dir(project.path())
        .args(["attach", "sluggy"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let resumed_runs_dir = home.path().join(".fabro").join("runs");
    let resumed_run_dir = std::fs::read_dir(&resumed_runs_dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.is_dir())
        .unwrap_or_else(|| {
            panic!(
                "expected a resumed run under {}",
                resumed_runs_dir.display()
            )
        });
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(resumed_run_dir.join("manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["workflow_name"].as_str(), Some("BarBaz"));
    assert_eq!(manifest["workflow_slug"].as_str(), Some("sluggy"));
}

#[test]
fn dry_run_create_start_attach_works_with_default_run_lookup() {
    let home = tempfile::tempdir().unwrap();
    let run_id = "drysplit-test-123";

    arc()
        .env("HOME", home.path())
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id,
            "../../../test/simple.fabro",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(run_id));

    let run_dir = find_run_dir(home.path(), run_id);
    assert!(
        run_dir.join("manifest.json").exists(),
        "create should persist manifest.json so the run is discoverable"
    );

    arc()
        .env("HOME", home.path())
        .args(["start", run_id])
        .assert()
        .success();

    arc()
        .env("HOME", home.path())
        .args(["attach", run_id])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    assert!(run_dir.join("conclusion.json").exists());
}

#[test]
fn dry_run_detach_attach_works_with_default_run_lookup() {
    let home = tempfile::tempdir().unwrap();
    let run_id = "drydetach-test-123";

    arc()
        .env("HOME", home.path())
        .args([
            "run",
            "--detach",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id,
            "../../../test/simple.fabro",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(run_id));

    arc()
        .env("HOME", home.path())
        .args(["attach", run_id])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();
}

#[test]
fn start_by_workflow_name_prefers_newly_created_submitted_run() {
    let home = tempfile::tempdir().unwrap();
    let old_run_dir = home.path().join(".fabro").join("runs").join("old-smoke");
    std::fs::create_dir_all(&old_run_dir).unwrap();
    std::fs::write(
        old_run_dir.join("manifest.json"),
        serde_json::json!({
            "run_id": "old-smoke",
            "workflow_name": "Smoke",
            "goal": "",
            "start_time": "2026-01-01T00:00:00Z",
            "node_count": 1,
            "edge_count": 0
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

    let run_id = "new-smoke-run-123";
    arc()
        .env("HOME", home.path())
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id,
            "smoke",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(run_id));

    arc()
        .env("HOME", home.path())
        .args(["start", "smoke"])
        .assert()
        .success();

    arc()
        .env("HOME", home.path())
        .args(["attach", run_id])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let new_run_dir = find_run_dir(home.path(), run_id);
    let status = std::fs::read_to_string(new_run_dir.join("status.json")).unwrap();
    assert!(
        status.contains("\"status\": \"succeeded\""),
        "expected the newly created Smoke run to be started and completed"
    );
}

// Bug 2: _run_engine should use cached graph.fabro, not spec.workflow_path.
// When the original workflow file is deleted between create and start,
// the engine should read the snapshot saved at create time.
#[test]
fn bug2_run_engine_uses_cached_graph_not_original_path() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = dir.path().join("run");
    std::fs::create_dir_all(&run_dir).unwrap();

    let dot = "\
digraph G {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare,  label=\"Exit\"]
  start -> exit
}";

    // spec.json: workflow_path points to a file that no longer exists
    let spec = serde_json::json!({
        "run_id": "test-bug2",
        "workflow_path": "/nonexistent/deleted-workflow.fabro",
        "dot_source": dot,
        "working_directory": run_dir.to_str().unwrap(),
        "goal": null,
        "model": "test-model",
        "provider": null,
        "sandbox_provider": "local",
        "labels": {},
        "verbose": false,
        "no_retro": true,

        "preserve_sandbox": false,
        "dry_run": true,
        "auto_approve": true
    });
    std::fs::write(
        run_dir.join("spec.json"),
        serde_json::to_string(&spec).unwrap(),
    )
    .unwrap();

    // The cached graph snapshot saved by `fabro create`
    std::fs::write(run_dir.join("graph.fabro"), dot).unwrap();

    // _run_engine should use graph.fabro and never reference the deleted file.
    // Bug: it reads spec.workflow_path → fails with file-not-found.
    let output = arc()
        .args(["_run_engine", "--run-dir", run_dir.to_str().unwrap()])
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

// Bug 3: attach loop must delete interview_request.json after handling it
// to prevent re-prompting the user on the next poll iteration.
#[test]
fn bug3_attach_leaves_interview_request_until_engine_consumes_response() {
    let home = tempfile::tempdir().unwrap();

    let run_dir = setup_run_dir(
        home.path(),
        "bug3-test",
        serde_json::json!({}),
        &[
            r#"{"ts":"2026-01-01T00:00:01Z","run_id":"bug3","event":"StageStarted","node_id":"gate","name":"Gate","index":0,"attempt":1,"max_attempts":1}"#,
        ],
    );

    // Status: running
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::json!({"status": "running", "updated_at": "2026-01-01T00:00:00Z"}).to_string(),
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
    std::fs::write(
        run_dir.join("interview_request.json"),
        serde_json::to_string(&question).unwrap(),
    )
    .unwrap();

    // Dead engine so attach exits after one iteration
    std::fs::write(run_dir.join("run.pid"), "99999999").unwrap();

    // Pipe "y\n" so ConsoleInterviewer doesn't block on stdin
    let _ = arc()
        .env("HOME", home.path())
        .env("NO_COLOR", "1")
        .args(["attach", "bug3-test"])
        .write_stdin("y\n")
        .timeout(std::time::Duration::from_secs(5))
        .output();

    // The attach loop should leave the request durable until the engine consumes
    // the response, so a crashed attach can be retried safely.
    assert!(
        run_dir.join("interview_request.json").exists(),
        "bug3: interview_request.json should stay present until the engine consumes the answer"
    );
    assert!(
        run_dir.join("interview_response.json").exists(),
        "bug3: attach should write interview_response.json after handling the prompt"
    );
    let response = std::fs::read_to_string(run_dir.join("interview_response.json")).unwrap();
    assert!(response.contains("\"value\": \"Yes\""));
}

#[test]
fn attach_closed_stdin_keeps_interview_pending() {
    let home = tempfile::tempdir().unwrap();

    let run_dir = setup_run_dir(
        home.path(),
        "attach-closed-stdin",
        serde_json::json!({}),
        &[
            r#"{"ts":"2026-01-01T00:00:01Z","run_id":"attach-closed-stdin","event":"StageStarted","node_id":"gate","name":"Gate","index":0,"attempt":1,"max_attempts":1}"#,
        ],
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
    std::fs::write(
        run_dir.join("interview_request.json"),
        serde_json::to_string(&question).unwrap(),
    )
    .unwrap();

    std::fs::write(run_dir.join("run.pid"), "99999999").unwrap();

    let assert = arc()
        .env("HOME", home.path())
        .env("NO_COLOR", "1")
        .args(["attach", "attach-closed-stdin"])
        .timeout(std::time::Duration::from_secs(5))
        .assert()
        .failure();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("still waiting for input"),
        "attach should explain that the run is still waiting for a human answer.\nstderr: {stderr}"
    );
    assert!(
        run_dir.join("interview_request.json").exists(),
        "attach with closed stdin must leave the request pending"
    );
    assert!(
        !run_dir.join("interview_response.json").exists(),
        "attach with closed stdin must not fabricate a response"
    );
    assert!(
        !run_dir.join("interview_request.claim").exists(),
        "attach with closed stdin must release the claim so a later attach can answer"
    );

    let progress = std::fs::read_to_string(run_dir.join("progress.jsonl")).unwrap();
    assert!(
        progress.contains("\"event\":\"RunNotice\"")
            && progress.contains("\"code\":\"interview_unanswered\""),
        "attach should emit a structured warning when the interview ends without an answer.\nprogress: {progress}"
    );
}

// Bug 4: attach should respect the verbose flag from spec.json.
// Currently ProgressUI is created with verbose=false regardless of spec.
#[test]
fn bug4_attach_respects_verbose_from_spec() {
    let home = tempfile::tempdir().unwrap();

    // Use pre-rename field names so handle_json_line can parse them
    // (isolates this test from bug 1). With 2 turns and 1 tool call,
    // verbose mode should display "(2 turns, 1 tools, …)" in the output.
    let run_dir = setup_run_dir(
        home.path(),
        "bug4-test",
        serde_json::json!({"verbose": true}),
        &[
            r#"{"ts":"2026-01-01T12:00:00Z","run_id":"bug4","event":"StageStarted","node_id":"code","name":"Code","index":0,"attempt":1,"max_attempts":1}"#,
            r#"{"ts":"2026-01-01T12:00:01Z","run_id":"bug4","event":"Agent.AssistantMessage","stage":"code","model":"claude-sonnet"}"#,
            r#"{"ts":"2026-01-01T12:00:02Z","run_id":"bug4","event":"Agent.AssistantMessage","stage":"code","model":"claude-sonnet"}"#,
            r#"{"ts":"2026-01-01T12:00:03Z","run_id":"bug4","event":"Agent.ToolCallStarted","stage":"code","tool_name":"read_file","tool_call_id":"tc1","arguments":{}}"#,
            r#"{"ts":"2026-01-01T12:00:04Z","run_id":"bug4","event":"Agent.ToolCallCompleted","stage":"code","tool_name":"read_file","tool_call_id":"tc1","is_error":false}"#,
            r#"{"ts":"2026-01-01T12:00:10Z","run_id":"bug4","event":"StageCompleted","node_id":"code","name":"Code","index":0,"duration_ms":10000,"status":"success","usage":{"input_tokens":1000,"output_tokens":500}}"#,
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
        .args(["attach", "bug4-test"])
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
