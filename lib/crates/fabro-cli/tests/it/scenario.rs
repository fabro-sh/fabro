use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::Command;
use fabro_store::RuntimeState;
use predicates;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[allow(deprecated)]
fn fabro() -> Command {
    let mut cmd = Command::cargo_bin("fabro").unwrap();
    cmd.env("NO_COLOR", "1");
    cmd
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../test/scenario")
        .join(name)
}

fn fixture_root(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../test")
        .join(name)
}

fn read_json(path: &Path) -> Value {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
}

fn read_checkpoint(run_dir: &Path) -> Value {
    read_json(&run_dir.join("checkpoint.json"))
}

fn read_conclusion(run_dir: &Path) -> Value {
    read_json(&run_dir.join("conclusion.json"))
}

/// Find the single run directory under `storage_dir/runs/`.
fn find_run_dir(storage_dir: &Path) -> PathBuf {
    let runs_base = storage_dir.join("runs");
    let entries: Vec<_> = std::fs::read_dir(&runs_base)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", runs_base.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one run directory under {}",
        runs_base.display()
    );
    entries[0].path()
}

fn completed_nodes(run_dir: &Path) -> Vec<String> {
    let cp = read_checkpoint(run_dir);
    cp["completed_nodes"]
        .as_array()
        .expect("completed_nodes should be an array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

fn has_event(run_dir: &Path, event_name: &str) -> bool {
    let path = run_dir.join("progress.jsonl");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read progress.jsonl: {e}"));
    content.lines().any(|line| {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            v["event"].as_str() == Some(event_name)
        } else {
            false
        }
    })
}

// ---------------------------------------------------------------------------
// Macro: generate local_* and daytona_* variants for each scenario
// ---------------------------------------------------------------------------

macro_rules! scenario_tests {
    ($name:ident) => {
        paste::paste! {
            #[test]
            #[ignore = "scenario: requires local sandbox"]
            fn [<local_ $name>]() {
                [<scenario_ $name>]("local");
            }

            #[test]
            #[ignore = "scenario: requires DAYTONA_API_KEY"]
            fn [<daytona_ $name>]() {
                [<scenario_ $name>]("daytona");
            }
        }
    };
}

fn timeout_for(sandbox: &str) -> Duration {
    match sandbox {
        "daytona" => Duration::from_secs(600),
        _ => Duration::from_secs(180),
    }
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

// 1. command_pipeline — two command nodes in sequence, no LLM
scenario_tests!(command_pipeline);

fn scenario_command_pipeline(sandbox: &str) {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().unwrap();
    let storage_dir = tmp.path().join("storage");

    // Validate the workflow before running it
    fabro()
        .args([
            "validate",
            fixture("command_pipeline.fabro").to_str().unwrap(),
        ])
        .assert()
        .success();

    fabro()
        .args([
            "run",
            "--auto-approve",
            "--no-retro",
            "--sandbox",
            sandbox,
            "--storage-dir",
            storage_dir.to_str().unwrap(),
            fixture("command_pipeline.fabro").to_str().unwrap(),
        ])
        .timeout(timeout_for(sandbox))
        .assert()
        .success();

    let run_dir = find_run_dir(&storage_dir);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(
        conclusion["status"].as_str(),
        Some("success"),
        "conclusion status should be success"
    );

    let nodes = completed_nodes(&run_dir);
    assert!(
        nodes.contains(&"step1".to_string()),
        "step1 should be completed"
    );
    assert!(
        nodes.contains(&"step2".to_string()),
        "step2 should be completed"
    );

    // Verify step1 stdout
    let stdout1 = std::fs::read_to_string(run_dir.join("nodes/step1/stdout.log"))
        .expect("step1 stdout.log should exist");
    assert!(
        stdout1.contains("hello-from-step1"),
        "step1 stdout should contain hello-from-step1, got: {stdout1}"
    );
}

// 2. conditional_branching — command + diamond gate, success path taken
scenario_tests!(conditional_branching);

fn scenario_conditional_branching(sandbox: &str) {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().unwrap();
    let storage_dir = tmp.path().join("storage");

    fabro()
        .args([
            "run",
            "--auto-approve",
            "--no-retro",
            "--sandbox",
            sandbox,
            "--storage-dir",
            storage_dir.to_str().unwrap(),
            fixture("conditional_branching.fabro").to_str().unwrap(),
        ])
        .timeout(timeout_for(sandbox))
        .assert()
        .success();

    let run_dir = find_run_dir(&storage_dir);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(conclusion["status"].as_str(), Some("success"));

    let nodes = completed_nodes(&run_dir);
    assert!(
        nodes.contains(&"passed".to_string()),
        "passed node should be in completed_nodes: {nodes:?}"
    );
    assert!(
        !nodes.contains(&"failed".to_string()),
        "failed node should NOT be in completed_nodes: {nodes:?}"
    );
}

// 3. agent_linear — single agent node with LLM
scenario_tests!(agent_linear);

fn scenario_agent_linear(sandbox: &str) {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().unwrap();
    let storage_dir = tmp.path().join("storage");

    fabro()
        .args([
            "run",
            "--auto-approve",
            "--no-retro",
            "--sandbox",
            sandbox,
            "--model",
            "claude-haiku-4-5",
            "--storage-dir",
            storage_dir.to_str().unwrap(),
            fixture("agent_linear.fabro").to_str().unwrap(),
        ])
        .timeout(timeout_for(sandbox))
        .assert()
        .success();

    let run_dir = find_run_dir(&storage_dir);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(conclusion["status"].as_str(), Some("success"));

    let nodes = completed_nodes(&run_dir);
    assert!(
        nodes.contains(&"work".to_string()),
        "work should be completed"
    );

    // Agent node should produce prompt.md and response.md
    let prompt_path = run_dir.join("nodes/work/prompt.md");
    assert!(prompt_path.exists(), "nodes/work/prompt.md should exist");

    let response_path = run_dir.join("nodes/work/response.md");
    assert!(
        response_path.exists(),
        "nodes/work/response.md should exist"
    );
    let response = std::fs::read_to_string(&response_path).unwrap();
    assert!(!response.is_empty(), "response.md should not be empty");
}

// 4. human_gate — human gate with --auto-approve selects first edge
scenario_tests!(human_gate);

fn scenario_human_gate(sandbox: &str) {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().unwrap();
    let storage_dir = tmp.path().join("storage");

    fabro()
        .args([
            "run",
            "--auto-approve",
            "--no-retro",
            "--sandbox",
            sandbox,
            "--model",
            "claude-haiku-4-5",
            "--storage-dir",
            storage_dir.to_str().unwrap(),
            fixture("human_gate.fabro").to_str().unwrap(),
        ])
        .timeout(timeout_for(sandbox))
        .assert()
        .success();

    let run_dir = find_run_dir(&storage_dir);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(conclusion["status"].as_str(), Some("success"));

    let nodes = completed_nodes(&run_dir);
    assert!(
        nodes.contains(&"ship".to_string()),
        "ship should be in completed_nodes (auto-approve picks first edge): {nodes:?}"
    );
    assert!(
        !nodes.contains(&"revise".to_string()),
        "revise should NOT be in completed_nodes: {nodes:?}"
    );
}

// 5. command_agent_mixed — command writes file, agent reads it, command verifies
scenario_tests!(command_agent_mixed);

fn scenario_command_agent_mixed(sandbox: &str) {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().unwrap();
    let storage_dir = tmp.path().join("storage");

    fabro()
        .args([
            "run",
            "--auto-approve",
            "--no-retro",
            "--sandbox",
            sandbox,
            "--model",
            "claude-haiku-4-5",
            "--storage-dir",
            storage_dir.to_str().unwrap(),
            fixture("command_agent_mixed.fabro").to_str().unwrap(),
        ])
        .timeout(timeout_for(sandbox))
        .assert()
        .success();

    let run_dir = find_run_dir(&storage_dir);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(conclusion["status"].as_str(), Some("success"));

    let nodes = completed_nodes(&run_dir);
    assert!(
        nodes.contains(&"setup".to_string()),
        "setup should be completed"
    );
    assert!(
        nodes.contains(&"work".to_string()),
        "work should be completed"
    );
    assert!(
        nodes.contains(&"verify".to_string()),
        "verify should be completed"
    );

    // Verify command node saw the flag
    let stdout = std::fs::read_to_string(run_dir.join("nodes/verify/stdout.log"))
        .expect("verify stdout.log should exist");
    assert!(
        stdout.contains("SCENARIO_FLAG_42"),
        "verify stdout should contain SCENARIO_FLAG_42, got: {stdout}"
    );
}

// 6. full_stack — command + agent + human gate + goal_gate, kitchen sink
scenario_tests!(full_stack);

fn scenario_full_stack(sandbox: &str) {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().unwrap();
    let storage_dir = tmp.path().join("storage");

    fabro()
        .args([
            "run",
            "--auto-approve",
            "--no-retro",
            "--sandbox",
            sandbox,
            "--model",
            "claude-haiku-4-5",
            "--storage-dir",
            storage_dir.to_str().unwrap(),
            fixture("full_stack.fabro").to_str().unwrap(),
        ])
        .timeout(timeout_for(sandbox))
        .assert()
        .success();

    let run_dir = find_run_dir(&storage_dir);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(
        conclusion["status"].as_str(),
        Some("success"),
        "conclusion: {conclusion}"
    );
    assert!(
        conclusion["duration_ms"].as_u64().unwrap_or(0) > 0,
        "duration_ms should be > 0"
    );

    // RunRecord should have key fields
    let run_record = read_json(&run_dir.join("run.json"));
    assert!(
        run_record["run_id"].as_str().is_some(),
        "run record should have run_id"
    );
    assert!(
        run_record["graph"]["name"].as_str().is_some(),
        "run record should have graph.name"
    );

    // Progress events
    assert!(
        has_event(&run_dir, "WorkflowRunStarted"),
        "progress should contain WorkflowRunStarted"
    );
    assert!(
        has_event(&run_dir, "WorkflowRunCompleted"),
        "progress should contain WorkflowRunCompleted"
    );

    // All expected nodes completed
    let nodes = completed_nodes(&run_dir);
    for expected in &["setup", "plan", "approve", "impl", "verify"] {
        assert!(
            nodes.contains(&expected.to_string()),
            "{expected} should be in completed_nodes: {nodes:?}"
        );
    }

    // Verify node stdout should contain PASS
    let stdout = std::fs::read_to_string(run_dir.join("nodes/verify/stdout.log"))
        .expect("verify stdout.log should exist");
    assert!(
        stdout.contains("PASS"),
        "verify stdout should contain PASS, got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// repo deinit
// ---------------------------------------------------------------------------

fn init_git_repo(path: &Path) {
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("git init should succeed");
}

fn init_fabro_project(path: &Path) {
    std::fs::write(path.join("fabro.toml"), "version = 1\n").unwrap();
    let workflow_dir = path.join("fabro/workflows/hello");
    std::fs::create_dir_all(&workflow_dir).unwrap();
    std::fs::write(workflow_dir.join("workflow.fabro"), "digraph {}").unwrap();
    std::fs::write(workflow_dir.join("workflow.toml"), "version = 1\n").unwrap();
}

#[test]
fn test_repo_deinit_removes_fabro_toml_and_dir() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    init_fabro_project(tmp.path());

    assert!(tmp.path().join("fabro.toml").exists());
    assert!(tmp.path().join("fabro").exists());

    fabro()
        .args(["repo", "deinit"])
        .current_dir(tmp.path())
        .assert()
        .success();

    assert!(
        !tmp.path().join("fabro.toml").exists(),
        "fabro.toml should be removed"
    );
    assert!(
        !tmp.path().join("fabro").exists(),
        "fabro/ directory should be removed"
    );
}

#[test]
fn test_repo_deinit_fails_when_not_initialized() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());

    fabro()
        .args(["repo", "deinit"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("not initialized"));
}

// ---------------------------------------------------------------------------
// repo init --skill
// ---------------------------------------------------------------------------

#[test]
fn test_repo_init_skill_installs_skill_files() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());

    fabro()
        .args(["repo", "init", "--skill"])
        .current_dir(tmp.path())
        .assert()
        .success();

    // Skill files should be installed under .claude/skills/fabro-create-workflow/
    let skill_dir = tmp.path().join(".claude/skills/fabro-create-workflow");
    assert!(skill_dir.join("SKILL.md").exists(), "SKILL.md should exist");
    assert!(
        skill_dir.join("references/dot-language.md").exists(),
        "dot-language.md should exist"
    );
}

#[test]
fn test_repo_init_help_does_not_show_skill() {
    let out = fabro().args(["repo", "init", "--help"]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("--skill"),
        "--skill should be hidden from help"
    );
}

// ---------------------------------------------------------------------------
// secret subcommand lifecycle
// ---------------------------------------------------------------------------

#[test]
fn test_secret_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();

    let secret = |args: &[&str]| -> assert_cmd::assert::Assert {
        fabro().env("HOME", tmp.path()).args(args).assert()
    };

    // 1. set FOO=bar
    secret(&["secret", "set", "FOO", "bar"]).success();

    // 2. get FOO → stdout is "bar\n"
    secret(&["secret", "get", "FOO"]).success().stdout("bar\n");

    // 3. list → contains FOO
    secret(&["secret", "list"])
        .success()
        .stdout(predicates::str::contains("FOO"));

    // 4. update FOO
    secret(&["secret", "set", "FOO", "updated"]).success();

    // 5. get FOO → "updated\n"
    secret(&["secret", "get", "FOO"])
        .success()
        .stdout("updated\n");

    // 6. rm FOO
    secret(&["secret", "rm", "FOO"]).success();

    // 7. get FOO → fails
    secret(&["secret", "get", "FOO"]).failure();
}

#[test]
fn test_secret_list_show_values() {
    let tmp = tempfile::tempdir().unwrap();

    let secret = |args: &[&str]| -> assert_cmd::assert::Assert {
        fabro().env("HOME", tmp.path()).args(args).assert()
    };

    secret(&["secret", "set", "A", "1"]).success();
    secret(&["secret", "set", "B", "2"]).success();

    // Without --show-values: just keys
    let out = secret(&["secret", "list"]).success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("A"));
    assert!(stdout.contains("B"));
    assert!(!stdout.contains("A=1"));

    // With --show-values: KEY=VALUE
    secret(&["secret", "list", "--show-values"])
        .success()
        .stdout(predicates::str::contains("A=1"))
        .stdout(predicates::str::contains("B=2"));
}

#[test]
fn test_secret_list_alias_ls() {
    let tmp = tempfile::tempdir().unwrap();

    fabro()
        .env("HOME", tmp.path())
        .args(["secret", "set", "X", "y"])
        .assert()
        .success();

    fabro()
        .env("HOME", tmp.path())
        .args(["secret", "ls"])
        .assert()
        .success()
        .stdout(predicates::str::contains("X"));
}

#[test]
fn test_secret_get_missing_key() {
    let tmp = tempfile::tempdir().unwrap();

    fabro()
        .env("HOME", tmp.path())
        .args(["secret", "get", "NOPE"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("secret not found"));
}

#[test]
fn test_secret_rm_missing_key() {
    let tmp = tempfile::tempdir().unwrap();

    fabro()
        .env("HOME", tmp.path())
        .args(["secret", "rm", "NOPE"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("secret not found"));
}

#[test]
fn test_secret_value_with_equals() {
    let tmp = tempfile::tempdir().unwrap();

    fabro()
        .env("HOME", tmp.path())
        .args(["secret", "set", "URL", "https://x.com?a=1&b=2"])
        .assert()
        .success();

    fabro()
        .env("HOME", tmp.path())
        .args(["secret", "get", "URL"])
        .assert()
        .success()
        .stdout("https://x.com?a=1&b=2\n");
}

// ---------------------------------------------------------------------------
// Standalone tests (no sandbox parametrization)
// ---------------------------------------------------------------------------

#[test]
fn test_validate_rejects_invalid() {
    fabro()
        .args(["validate", fixture_root("invalid.fabro").to_str().unwrap()])
        .assert()
        .failure();
}

#[test]
fn test_model_list() {
    fabro()
        .args(["model", "list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("claude-haiku"));
}

#[test]
fn test_workflow_list() {
    let tmp = tempfile::tempdir().unwrap();

    // Minimal project structure: fabro.toml + a workflow
    std::fs::write(tmp.path().join("fabro.toml"), "version = 1\n").unwrap();
    let wf_dir = tmp.path().join("workflows/my_test_wf");
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(
        wf_dir.join("workflow.toml"),
        "version = 1\ngoal = \"A test workflow\"\n",
    )
    .unwrap();

    fabro()
        .args(["workflow", "list"])
        .current_dir(tmp.path())
        .assert()
        .success()
        // workflow list prints to stderr
        .stderr(predicates::str::contains("my_test_wf"));
}

#[test]
#[ignore = "scenario: requires ANTHROPIC_API_KEY"]
fn test_doctor() {
    dotenvy::dotenv().ok();
    fabro().args(["doctor"]).assert().success();
}

// ---------------------------------------------------------------------------
// Run lifecycle: ps / inspect / logs / rm / system df
// ---------------------------------------------------------------------------

#[test]
#[ignore = "scenario: requires local sandbox"]
fn local_run_lifecycle() {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().unwrap();

    let fabro_home = |args: &[&str]| -> assert_cmd::assert::Assert {
        fabro()
            .env("HOME", tmp.path())
            .args(args)
            .timeout(timeout_for("local"))
            .assert()
    };

    // 1. Run a workflow — run lands under <tmp>/.fabro/runs/
    fabro_home(&[
        "run",
        "--auto-approve",
        "--no-retro",
        "--no-upgrade-check",
        "--sandbox",
        "local",
        fixture("command_pipeline.fabro").to_str().unwrap(),
    ])
    .success();

    // 2. ps -a --json — should list exactly one run
    let ps_out = fabro_home(&["ps", "-a", "--json"]).success();
    let ps_stdout = String::from_utf8(ps_out.get_output().stdout.clone()).unwrap();
    let runs: Vec<Value> =
        serde_json::from_str(&ps_stdout).expect("ps --json should produce a JSON array");
    assert_eq!(runs.len(), 1, "should have exactly one run: {ps_stdout}");
    let run_id = runs[0]["run_id"]
        .as_str()
        .expect("run should have run_id")
        .to_string();
    assert_eq!(
        runs[0]["workflow_name"].as_str(),
        Some("CommandPipeline"),
        "workflow_name should be CommandPipeline"
    );

    // 3. inspect <run_id> — JSON array with run_record and conclusion
    let inspect_out = fabro_home(&["inspect", &run_id]).success();
    let inspect_stdout = String::from_utf8(inspect_out.get_output().stdout.clone()).unwrap();
    let items: Vec<Value> =
        serde_json::from_str(&inspect_stdout).expect("inspect should produce a JSON array");
    assert!(!items.is_empty(), "inspect should return at least one item");
    assert!(
        items[0]["run_record"].is_object(),
        "inspect should include run_record"
    );
    assert!(
        items[0]["conclusion"].is_object(),
        "inspect should include conclusion"
    );
    let run_dir = PathBuf::from(
        items[0]["run_dir"]
            .as_str()
            .expect("inspect should include run_dir"),
    );

    // 4. logs <run_id> — non-empty, first line is valid JSONL with event field
    let logs_out = fabro_home(&["logs", &run_id]).success();
    let logs_stdout = String::from_utf8(logs_out.get_output().stdout.clone()).unwrap();
    assert!(!logs_stdout.is_empty(), "logs should not be empty");
    let first_line = logs_stdout.lines().next().unwrap();
    let log_entry: Value =
        serde_json::from_str(first_line).expect("first log line should be valid JSON");
    assert!(
        log_entry["event"].is_string(),
        "first log line should have an event field"
    );

    // 5. asset list — no assets yet, should succeed with empty message
    let asset_list_out = fabro_home(&["asset", "list", &run_id]).success();
    let asset_list_stdout = String::from_utf8(asset_list_out.get_output().stdout.clone()).unwrap();
    assert!(
        asset_list_stdout.contains("No assets found"),
        "asset list should report no assets: {asset_list_stdout}"
    );

    // 6. Seed a synthetic asset so asset list/cp have something to work with.
    let asset_dir = RuntimeState::new(&run_dir).asset_stage_dir("step1", 0);
    std::fs::create_dir_all(&asset_dir).unwrap();
    std::fs::write(asset_dir.join("output.txt"), "asset-content-42").unwrap();
    std::fs::write(
        asset_dir.join("manifest.json"),
        r#"{"files_copied":1,"total_bytes":16,"files_skipped":0,"download_errors":0,"hash_errors":0,"captured_assets":[{"path":"output.txt","mime":"text/plain","content_md5":"f02439728c0a94b7bfc465acb1201a1f","content_sha256":"0af9dea3e1c2dec968531c18c9331659b8268e8c9cf24b01cda7b8ce51d2ff00","bytes":16}]}"#,
    )
    .unwrap();

    // 7. asset list — now shows the seeded asset
    let asset_list_out2 = fabro_home(&["asset", "list", &run_id, "--json"]).success();
    let asset_list_stdout2 =
        String::from_utf8(asset_list_out2.get_output().stdout.clone()).unwrap();
    let assets: Vec<Value> = serde_json::from_str(&asset_list_stdout2)
        .expect("asset list --json should produce a JSON array");
    assert_eq!(
        assets.len(),
        1,
        "should have one asset: {asset_list_stdout2}"
    );
    assert_eq!(assets[0]["relative_path"].as_str(), Some("output.txt"));
    assert_eq!(assets[0]["node_slug"].as_str(), Some("step1"));

    // 8. asset cp — copy the asset out
    let asset_dest = tmp.path().join("asset_copy");
    fabro_home(&[
        "asset",
        "cp",
        &format!("{run_id}:output.txt"),
        asset_dest.to_str().unwrap(),
    ])
    .success();
    let copied = std::fs::read_to_string(asset_dest.join("output.txt")).unwrap();
    assert_eq!(
        copied, "asset-content-42",
        "asset cp should copy file content"
    );

    // 9. cp — download a file from the local sandbox workdir
    let sandbox_json: Value = read_json(&run_dir.join("sandbox.json"));
    let workdir = sandbox_json["working_directory"]
        .as_str()
        .expect("sandbox.json should have working_directory");
    // Plant a file in the sandbox workdir so we can download it
    std::fs::write(
        PathBuf::from(workdir).join("cp_test.txt"),
        "downloaded-via-cp",
    )
    .unwrap();
    let cp_dest = tmp.path().join("cp_download.txt");
    fabro_home(&[
        "cp",
        &format!("{run_id}:cp_test.txt"),
        cp_dest.to_str().unwrap(),
    ])
    .success();
    let cp_content = std::fs::read_to_string(&cp_dest).unwrap();
    assert_eq!(
        cp_content, "downloaded-via-cp",
        "cp should download file from sandbox"
    );

    // 10. system df — mentions "Runs"
    let df_out = fabro_home(&["system", "df"]).success();
    let df_stdout = String::from_utf8(df_out.get_output().stdout.clone()).unwrap();
    assert!(
        df_stdout.contains("Runs"),
        "system df should mention Runs: {df_stdout}"
    );

    // 11. rm <run_id> — remove the run
    fabro_home(&["rm", &run_id]).success();

    // 12. ps -a --json — should be empty
    let ps_out2 = fabro_home(&["ps", "-a", "--json"]).success();
    let ps_stdout2 = String::from_utf8(ps_out2.get_output().stdout.clone()).unwrap();
    let runs2: Vec<Value> =
        serde_json::from_str(&ps_stdout2).expect("ps --json should produce a JSON array");
    assert!(
        runs2.is_empty(),
        "runs should be empty after rm: {ps_stdout2}"
    );
}

// ---------------------------------------------------------------------------
// exec — exercises `fabro exec`
// ---------------------------------------------------------------------------

#[test]
#[ignore = "scenario: requires ANTHROPIC_API_KEY"]
fn test_exec_creates_file() {
    dotenvy::dotenv().ok();
    let tmp = tempfile::tempdir().unwrap();

    fabro()
        .args([
            "exec",
            "--auto-approve",
            "--permissions",
            "full",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "Create a file called hello.txt containing exactly 'Hello from exec scenario'",
        ])
        .current_dir(tmp.path())
        .timeout(Duration::from_secs(120))
        .assert()
        .success();

    let hello = tmp.path().join("hello.txt");
    assert!(hello.exists(), "hello.txt should exist after exec");
    let content = std::fs::read_to_string(&hello).unwrap();
    assert!(
        content.contains("Hello from exec scenario"),
        "hello.txt should contain greeting, got: {content}"
    );
}
