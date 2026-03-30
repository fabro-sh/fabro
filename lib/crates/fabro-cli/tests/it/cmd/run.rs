use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::TimeZone;
use fabro_config::FabroSettings;
use fabro_git_storage::branchstore::BranchStore;
use fabro_git_storage::gitobj::Store as GitStore;
use fabro_store::{NodeVisitRef, RuntimeState, SlateStore, Store as _};
use fabro_test::{TestContext, fabro_snapshot, test_context};
use fabro_types::{Checkpoint, Graph, RunRecord, StartRecord};
use git2::{Repository, Signature};
use object_store::local::LocalFileSystem;
use serde_json::Value;
use tokio::runtime::Runtime;

macro_rules! fabro_json_snapshot {
    ($context:expr, $value:expr, @$snapshot:literal) => {{
        let mut filters = $context.filters();
        filters.push((
            r"\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z\b".to_string(),
            "[TIMESTAMP]".to_string(),
        ));
        let filters: Vec<(&str, &str)> = filters
            .iter()
            .map(|(pattern, replacement)| (pattern.as_str(), replacement.as_str()))
            .collect();
        let rendered = serde_json::to_string_pretty(&$value).unwrap();
        insta::with_settings!({ filters => filters }, {
            insta::assert_snapshot!(rendered, @$snapshot);
        });
    }};
}

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../../test/{name}"))
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

async fn seed_durable_run(storage_dir: &Path, repo_dir: &Path, run_id: &str) {
    let store_path = storage_dir.join("store");
    std::fs::create_dir_all(&store_path).unwrap();
    let object_store = Arc::new(LocalFileSystem::new_with_prefix(&store_path).unwrap());
    let store = SlateStore::new(object_store, "", Duration::from_millis(5));
    let run_id: fabro_types::RunId = run_id.parse().unwrap();
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

fn read_json(path: impl AsRef<Path>) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn read_jsonl(path: impl AsRef<Path>) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn compact_progress_event(event: &Value) -> Value {
    let mut compact = serde_json::Map::new();
    for key in [
        "event",
        "sandbox_provider",
        "workflow_name",
        "goal",
        "node_id",
        "node_label",
        "handler_type",
        "stage_index",
        "status",
        "from_node_id",
        "to_node_id",
        "reason",
        "artifact_count",
    ] {
        if let Some(value) = event.get(key).filter(|value| !value.is_null()) {
            compact.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(compact)
}

fn run_output_filters(context: &TestContext) -> Vec<(String, String)> {
    let mut filters = context.filters();
    filters.push((r"\b\d+ms\b".to_string(), "[TIME]".to_string()));
    filters
}

/// Helper: create a minimal run directory that `resolve_run` can find.
/// Sets up run.json, status.json, and progress.jsonl.
fn setup_run_dir(
    storage_dir: &std::path::Path,
    run_id: &str,
    spec_overrides: serde_json::Value,
    progress_lines: &[&str],
) -> std::path::PathBuf {
    let run_dir = storage_dir.join("runs").join(run_id);
    std::fs::create_dir_all(&run_dir).unwrap();

    // Build defaults, then merge overrides
    let overrides = spec_overrides;
    let get_str = |key: &str, default: &str| -> serde_json::Value {
        overrides
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| serde_json::json!(s))
            .unwrap_or_else(|| serde_json::json!(default))
    };
    let get_bool = |key: &str, default: bool| -> serde_json::Value {
        overrides
            .get(key)
            .and_then(|v| v.as_bool())
            .map(|b| serde_json::json!(b))
            .unwrap_or_else(|| serde_json::json!(default))
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

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Launch a workflow run

    Usage: fabro run [OPTIONS] <WORKFLOW>

    Arguments:
      <WORKFLOW>  Path to a .fabro workflow file or .toml task config

    Options:
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --dry-run                    Execute with simulated LLM backend
          --auto-approve               Auto-approve all human gates
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --goal <GOAL>                Override the workflow goal (exposed as $goal in prompts)
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --goal-file <GOAL_FILE>      Read the workflow goal from a file
          --model <MODEL>              Override default LLM model
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --provider <PROVIDER>        Override default LLM provider
      -v, --verbose                    Enable verbose output
          --sandbox <SANDBOX>          Sandbox for agent tools [possible values: local, docker, daytona]
          --label <KEY=VALUE>          Attach a label to this run (repeatable, format: KEY=VALUE)
          --no-retro                   Skip retro generation after the run
          --preserve-sandbox           Keep the sandbox alive after the run finishes (for debugging)
      -d, --detach                     Run the workflow in the background and print the run ID
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn dry_run_simple() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(fixture("simple.fabro"));
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Simple (4 nodes, 3 edges)
    Graph: ../../../test/simple.fabro
    Goal: Run tests and report results

        Sandbox: local (ready in [TIME])
        ✓ Start  [TIME]
        ✓ Run Tests  [TIME]
        ✓ Report  [TIME]
        ✓ Exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  [DURATION]
    Run:       [DRY_RUN_DIR]

    === Output ===
    [Simulated] Response for stage: report
    ");
}

#[test]
fn dry_run_branching() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(fixture("branching.fabro"));
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Branch (6 nodes, 6 edges)
    Graph: ../../../test/branching.fabro
    Goal: Implement and validate a feature

    warning [node: implement]: Node 'implement' has goal_gate=true but no retry_target or fallback_retry_target (goal_gate_has_retry)
        Sandbox: local (ready in [TIME])
        ✓ Start  [TIME]
        ✓ Plan  [TIME]
        ✓ Implement  [TIME]
        ✓ Validate  [TIME]
        ✓ Tests passing?  [TIME]
        ✓ Exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  [DURATION]
    Run:       [DRY_RUN_DIR]

    === Output ===
    [Simulated] Response for stage: validate
    ");
}

#[test]
fn dry_run_conditions() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(fixture("conditions.fabro"));
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Conditions (5 nodes, 5 edges)
    Graph: ../../../test/conditions.fabro
    Goal: Test condition evaluation with OR and parentheses

        Sandbox: local (ready in [TIME])
        ✓ start  [TIME]
        ✓ Decide  [TIME]
        ✓ Path B  [TIME]
        ✓ exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  [DURATION]
    Run:       [DRY_RUN_DIR]

    === Output ===
    [Simulated] Response for stage: path_b
    ");
}

#[test]
fn dry_run_parallel() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(fixture("parallel.fabro"));
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Parallel (7 nodes, 7 edges)
    Graph: ../../../test/parallel.fabro
    Goal: Test parallel and fan-in execution

        Sandbox: local (ready in [TIME])
        ✓ start  [TIME]
        ✓ Fork Work  [TIME]
        ✓ Merge Results  [TIME]
        ✓ Review  [TIME]
        ✓ exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  [DURATION]
    Run:       [DRY_RUN_DIR]

    === Output ===
    [Simulated] Response for stage: review
    ");
}

#[test]
fn dry_run_styled() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(fixture("styled.fabro"));
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Styled (5 nodes, 4 edges)
    Graph: ../../../test/styled.fabro
    Goal: Build a styled pipeline

        Sandbox: local (ready in [TIME])
        ✓ start  [TIME]
        ✓ Plan  [TIME]
        ✓ Implement  [TIME]
        ✓ Critical Review  [TIME]
        ✓ exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  [DURATION]
    Run:       [DRY_RUN_DIR]

    === Output ===
    [Simulated] Response for stage: critical_review
    ");
}

#[test]
fn dry_run_legacy_tool() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(fixture("legacy_tool.fabro"));
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: LegacyTool (3 nodes, 2 edges)
    Graph: ../../../test/legacy_tool.fabro
    Goal: Verify backwards compatibility with old tool naming

        Sandbox: local (ready in [TIME])
        ✓ Start  [TIME]
        ✓ Echo  [TIME]
        ✓ Exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  [DURATION]
    Run:       [DRY_RUN_DIR]
    ");
}

#[test]
fn dry_run_writes_jsonl_and_live_json() {
    let context = test_context!();
    let run_id = "01ARZ3NDEKTSV4RRFFQ69G5FB8";

    context
        .command()
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id,
            "../../../test/simple.fabro",
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(run_id);
    let jsonl_path = run_dir.join("progress.jsonl");
    let progress = read_jsonl(&jsonl_path);
    assert!(
        !progress.is_empty(),
        "progress.jsonl should have at least one line"
    );
    let progress_summary: Vec<_> = progress.iter().map(compact_progress_event).collect();
    fabro_json_snapshot!(context, &progress_summary, @r#"
    [
      {
        "event": "Sandbox.Initializing",
        "sandbox_provider": "local"
      },
      {
        "event": "Sandbox.Ready",
        "sandbox_provider": "local"
      },
      {
        "event": "SandboxInitialized"
      },
      {
        "event": "WorkflowRunStarted",
        "workflow_name": "Simple",
        "goal": "Run tests and report results"
      },
      {
        "event": "StageStarted",
        "node_id": "start",
        "node_label": "Start",
        "handler_type": "start",
        "stage_index": 0
      },
      {
        "event": "StageCompleted",
        "node_id": "start",
        "node_label": "Start",
        "stage_index": 0,
        "status": "success"
      },
      {
        "event": "EdgeSelected",
        "from_node_id": "start",
        "to_node_id": "run_tests",
        "reason": "unconditional"
      },
      {
        "event": "CheckpointCompleted",
        "node_id": "start",
        "node_label": "start",
        "status": "success"
      },
      {
        "event": "StageStarted",
        "node_id": "run_tests",
        "node_label": "Run Tests",
        "handler_type": "agent",
        "stage_index": 1
      },
      {
        "event": "StageCompleted",
        "node_id": "run_tests",
        "node_label": "Run Tests",
        "stage_index": 1,
        "status": "success"
      },
      {
        "event": "EdgeSelected",
        "from_node_id": "run_tests",
        "to_node_id": "report",
        "reason": "unconditional"
      },
      {
        "event": "CheckpointCompleted",
        "node_id": "run_tests",
        "node_label": "run_tests",
        "status": "success"
      },
      {
        "event": "StageStarted",
        "node_id": "report",
        "node_label": "Report",
        "handler_type": "agent",
        "stage_index": 2
      },
      {
        "event": "StageCompleted",
        "node_id": "report",
        "node_label": "Report",
        "stage_index": 2,
        "status": "success"
      },
      {
        "event": "EdgeSelected",
        "from_node_id": "report",
        "to_node_id": "exit",
        "reason": "unconditional"
      },
      {
        "event": "CheckpointCompleted",
        "node_id": "report",
        "node_label": "report",
        "status": "success"
      },
      {
        "event": "StageStarted",
        "node_id": "exit",
        "node_label": "Exit",
        "handler_type": "exit",
        "stage_index": 3
      },
      {
        "event": "StageCompleted",
        "node_id": "exit",
        "node_label": "Exit",
        "stage_index": 3,
        "status": "success"
      },
      {
        "event": "WorkflowRunCompleted",
        "status": "success",
        "artifact_count": 0
      },
      {
        "event": "Sandbox.CleanupStarted",
        "sandbox_provider": "local"
      },
      {
        "event": "Sandbox.CleanupCompleted",
        "sandbox_provider": "local"
      }
    ]
    "#);

    let live_path = run_dir.join("live.json");
    let live_content = read_json(&live_path);
    let live_summary = compact_progress_event(&live_content);
    fabro_json_snapshot!(context, &live_summary, @r#"
    {
      "event": "Sandbox.CleanupCompleted",
      "sandbox_provider": "local"
    }
    "#);

    assert_eq!(live_summary, progress_summary.last().cloned().unwrap());
}

// == --run-id passthrough =====================================================

#[test]
fn run_id_passthrough_uses_provided_ulid() {
    let context = test_context!();
    let my_ulid = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let mut cmd = context.run_cmd();
    cmd.args([
        "--dry-run",
        "--auto-approve",
        "--run-id",
        my_ulid,
        "../../../test/simple.fabro",
    ]);
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Simple (4 nodes, 3 edges)
    Graph: ../../../test/simple.fabro
    Goal: Run tests and report results

        Sandbox: local (ready in [TIME])
        ✓ Start  [TIME]
        ✓ Run Tests  [TIME]
        ✓ Report  [TIME]
        ✓ Exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  [DURATION]
    Run:       [DRY_RUN_DIR]

    === Output ===
    [Simulated] Response for stage: report
    ");
}

#[test]
fn detach_prints_ulid_and_exits() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args([
        "--detach",
        "--dry-run",
        "--auto-approve",
        "../../../test/simple.fabro",
    ]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    [ULID]
    ----- stderr -----
    ");
}

#[test]
fn detach_creates_run_dir_with_detach_log() {
    let context = test_context!();
    let run_id = "01ARZ3NDEKTSV4RRFFQ69G5FB9";

    context
        .run_cmd()
        .args([
            "--detach",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id,
            "../../../test/simple.fabro",
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(run_id);
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "run_dir": run_dir,
            "launcher_log_exists": context.storage_dir.join("launchers").join(format!("{run_id}.log")).exists(),
            "detach_log_exists": run_dir.join("detach.log").exists(),
        }),
        @r#"
        {
          "run_dir": "[DRY_RUN_DIR]",
          "launcher_log_exists": true,
          "detach_log_exists": false
        }
        "#
    );
}

// == Resume ===================================================================

#[test]
fn resume_requires_run_arg() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["resume"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: the following required arguments were not provided:
      <RUN>

    Usage: fabro resume --no-upgrade-check --storage-dir <STORAGE_DIR> <RUN>

    For more information, try '--help'.
    ");
}

#[test]
fn rewind_and_fork_recover_missing_metadata_from_store() {
    let context = test_context!();
    let repo_dir = tempfile::tempdir().unwrap();
    Repository::init(repo_dir.path()).unwrap();

    let source_run_id = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    let expected_shas = seed_run_branch(repo_dir.path(), source_run_id, &["start", "build"]);
    Runtime::new().unwrap().block_on(seed_durable_run(
        &context.storage_dir,
        repo_dir.path(),
        source_run_id,
    ));
    let mut filters = context.filters();
    for (idx, sha) in expected_shas.iter().enumerate() {
        let replacement = format!("[SHA_{}]", idx + 1);
        filters.push((regex::escape(sha), replacement.clone()));
        filters.push((regex::escape(&sha[..8]), format!("[SHA_{}]", idx + 1)));
        filters.push((regex::escape(&sha[..7]), replacement));
    }

    assert!(
        list_metadata_run_ids(repo_dir.path()).is_empty(),
        "metadata branch should start missing"
    );

    let mut rewind_list = context.command();
    rewind_list.current_dir(repo_dir.path());
    rewind_list.args(["rewind", source_run_id, "--list"]);
    rewind_list.timeout(Duration::from_secs(15));
    fabro_snapshot!(filters.clone(), rewind_list, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    @   Node   Details 
     @1  start          
     @2  build
    ");

    let rebuilt_checkpoints = metadata_checkpoints(repo_dir.path(), source_run_id);
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
    context
        .command()
        .current_dir(repo_dir.path())
        .args(["fork", source_run_id, "--no-push"])
        .timeout(Duration::from_secs(15))
        .assert()
        .success();
    let after_child = list_metadata_run_ids(repo_dir.path());
    let child_run_ids: Vec<_> = after_child.difference(&before_child).cloned().collect();
    assert_eq!(child_run_ids.len(), 1, "expected one child run");
    let child_run_id = &child_run_ids[0];
    filters.push((regex::escape(child_run_id), "[CHILD_RUN_ID]".to_string()));
    filters.push((
        regex::escape(&child_run_id[..8]),
        "[CHILD_RUN_PREFIX]".to_string(),
    ));

    let child_checkpoint = latest_metadata_checkpoint(repo_dir.path(), child_run_id);
    assert_eq!(
        child_checkpoint.git_commit_sha.as_deref(),
        Some(expected_shas[1].as_str())
    );

    let mut child_rewind = context.command();
    child_rewind.current_dir(repo_dir.path());
    child_rewind.args(["rewind", child_run_id, "@1", "--no-push"]);
    child_rewind.timeout(Duration::from_secs(15));
    fabro_snapshot!(filters, child_rewind, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Rewound metadata branch to @1 (build)
    Rewound run branch fabro/run/[ULID] to [SHA_2]

    To resume: fabro resume [CHILD_RUN_PREFIX]
    ");

    let before_grandchild = after_child;
    context
        .command()
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

#[test]
fn completed_run_preserves_workflow_slug_for_lookup() {
    let context = test_context!();
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

    context
        .command()
        .current_dir(project.path())
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            "01ARZ3NDEKTSV4RRFFQ69G5FAX",
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    context
        .command()
        .current_dir(project.path())
        .args(["start", "sluggy"])
        .assert()
        .success();

    context
        .command()
        .current_dir(project.path())
        .args(["attach", "01ARZ3NDEKTSV4RRFFQ69G5FAX"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    context
        .command()
        .current_dir(project.path())
        .args(["attach", "sluggy"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let run_dir = context.find_run_dir("01ARZ3NDEKTSV4RRFFQ69G5FAX");
    let run_record = read_json(run_dir.join("run.json"));
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "run_id": run_record["run_id"],
            "graph_name": run_record["graph"]["name"],
            "workflow_slug": run_record["workflow_slug"],
        }),
        @r#"
        {
          "run_id": "[ULID]",
          "graph_name": "BarBaz",
          "workflow_slug": "sluggy"
        }
        "#
    );
}

#[test]
fn standalone_file_run_uses_file_stem_slug_for_lookup() {
    let context = test_context!();
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

    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            "01ARZ3NDEKTSV4RRFFQ69G5FAY",
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    context
        .command()
        .args(["start", "alpha"])
        .assert()
        .success();

    context
        .command()
        .args(["attach", "alpha"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let run_dir = context.find_run_dir("01ARZ3NDEKTSV4RRFFQ69G5FAY");
    let run_record = read_json(run_dir.join("run.json"));
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "run_id": run_record["run_id"],
            "graph_name": run_record["graph"]["name"],
            "workflow_slug": run_record["workflow_slug"],
        }),
        @r#"
        {
          "run_id": "[ULID]",
          "graph_name": "FooWorkflow",
          "workflow_slug": "alpha"
        }
        "#
    );
}

#[test]
fn dry_run_create_start_attach_works_with_default_run_lookup() {
    let run_id = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
    let context = test_context!();

    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id,
            "../../../test/simple.fabro",
        ])
        .assert()
        .success();

    let run_dir = context.find_run_dir(run_id);

    context.command().args(["start", run_id]).assert().success();

    context
        .command()
        .args(["attach", run_id])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "run_json_exists": run_dir.join("run.json").exists(),
            "conclusion_json_exists": run_dir.join("conclusion.json").exists(),
        }),
        @r#"
        {
          "run_json_exists": true,
          "conclusion_json_exists": true
        }
        "#
    );
}

#[test]
fn dry_run_detach_attach_works_with_default_run_lookup() {
    let run_id = "01ARZ3NDEKTSV4RRFFQ69G5FB0";
    let context = test_context!();

    context
        .command()
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
        .success();

    context
        .command()
        .args(["attach", run_id])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let run_dir = context.find_run_dir(run_id);
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "run_dir": run_dir,
            "conclusion_json_exists": run_dir.join("conclusion.json").exists(),
        }),
        @r#"
        {
          "run_dir": "[DRY_RUN_DIR]",
          "conclusion_json_exists": true
        }
        "#
    );
}

#[test]
fn start_by_workflow_name_prefers_newly_created_submitted_run() {
    let context = test_context!();
    let old_run_dir = context
        .storage_dir
        .join("runs")
        .join("01ARZ3NDEKTSV4RRFFQ69G5FB1");
    std::fs::create_dir_all(&old_run_dir).unwrap();
    std::fs::write(
        old_run_dir.join("run.json"),
        serde_json::json!({
            "run_id": "01ARZ3NDEKTSV4RRFFQ69G5FB1",
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

    let run_id = "01ARZ3NDEKTSV4RRFFQ69G5FB2";
    context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--run-id",
            run_id,
            "smoke",
        ])
        .assert()
        .success();

    context
        .command()
        .args(["start", "smoke"])
        .assert()
        .success();

    context
        .command()
        .args(["attach", run_id])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let new_run_dir = context.find_run_dir(run_id);
    let status = read_json(new_run_dir.join("status.json"));
    fabro_json_snapshot!(context, &status, @r#"
    {
      "status": "succeeded",
      "reason": "completed",
      "updated_at": "[TIMESTAMP]"
    }
    "#);
}

// Bug 2: __detached should use cached graph.fabro, not run.json working_directory.
// When the original workflow file is deleted between create and start,
// the engine should read the snapshot saved at create time.
#[test]
fn bug2_detached_uses_cached_graph_not_original_path() {
    let context = test_context!();
    let run_dir = context
        .storage_dir
        .join("runs")
        .join("01ARZ3NDEKTSV4RRFFQ69G5FB3");
    std::fs::create_dir_all(&run_dir).unwrap();

    let dot = "\
digraph G {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare,  label=\"Exit\"]
  start -> exit
}";

    // run.json: working_directory is valid but original workflow path no longer exists
    let run_record = serde_json::json!({
        "run_id": "01ARZ3NDEKTSV4RRFFQ69G5FB3",
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

    let mut cmd = context.command();
    cmd.args([
        "__detached",
        "--run-dir",
        run_dir.to_str().unwrap(),
        "--launcher-path",
        context
            .storage_dir
            .join("launchers")
            .join("01ARZ3NDEKTSV4RRFFQ69G5FB3.json")
            .to_str()
            .unwrap(),
    ]);
    cmd.timeout(std::time::Duration::from_secs(15));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    ");
}

#[test]
fn bug4_detached_resume_rejects_completed_run_without_mutating_it() {
    let context = test_context!();
    context.write_temp(
        "workflow.fabro",
        "\
digraph Test {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    );

    let run = context
        .command()
        .current_dir(&context.temp_dir)
        .args([
            "run",
            "--dry-run",
            "--auto-approve",
            "--no-retro",
            "--detach",
            context.temp_dir.join("workflow.fabro").to_str().unwrap(),
        ])
        .assert()
        .success();
    let run_id = String::from_utf8(run.get_output().stdout.clone())
        .unwrap()
        .trim()
        .to_string();

    context
        .command()
        .args(["wait", &run_id])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();

    let inspect_before = context
        .command()
        .args(["inspect", &run_id])
        .assert()
        .success();
    let before: serde_json::Value =
        serde_json::from_slice(&inspect_before.get_output().stdout).unwrap();
    let before_summary = serde_json::json!({
        "run_dir": before[0]["run_dir"],
        "start_time": before[0]["start_record"]["start_time"],
        "conclusion_timestamp": before[0]["conclusion"]["timestamp"],
        "conclusion_status": before[0]["conclusion"]["status"],
    });
    let run_dir = before_summary["run_dir"].as_str().unwrap().to_string();
    fabro_json_snapshot!(context, &before_summary, @r#"
    {
      "run_dir": "[DRY_RUN_DIR]",
      "start_time": "[TIMESTAMP]",
      "conclusion_timestamp": "[TIMESTAMP]",
      "conclusion_status": "success"
    }
    "#);

    let mut cmd = context.command();
    cmd.args([
        "__detached",
        "--run-dir",
        &run_dir,
        "--launcher-path",
        context
            .storage_dir
            .join("launchers")
            .join(format!("{run_id}.json"))
            .to_str()
            .unwrap(),
        "--resume",
    ]);
    cmd.timeout(std::time::Duration::from_secs(10));
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Precondition failed: run already finished successfully — nothing to resume
    ");

    let inspect_after = context
        .command()
        .args(["inspect", &run_id])
        .assert()
        .success();
    let after: serde_json::Value =
        serde_json::from_slice(&inspect_after.get_output().stdout).unwrap();
    let after_summary = serde_json::json!({
        "run_dir": after[0]["run_dir"],
        "start_time": after[0]["start_record"]["start_time"],
        "conclusion_timestamp": after[0]["conclusion"]["timestamp"],
        "conclusion_status": after[0]["conclusion"]["status"],
    });

    assert_eq!(after_summary, before_summary);
}

#[test]
fn bug5_detached_uses_snapshotted_app_id_for_github_credentials() {
    let context = test_context!();
    let run_dir = context
        .storage_dir
        .join("runs")
        .join("01ARZ3NDEKTSV4RRFFQ69G5FB4");
    std::fs::create_dir_all(&run_dir).unwrap();

    let dot = "\
digraph G {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare,  label=\"Exit\"]
  start -> exit
}";

    let run_record = serde_json::json!({
        "run_id": "01ARZ3NDEKTSV4RRFFQ69G5FB4",
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

    let mut cmd = context.command();
    cmd.env("GITHUB_APP_PRIVATE_KEY", "%%%not-base64%%%");
    cmd.args([
        "__detached",
        "--run-dir",
        run_dir.to_str().unwrap(),
        "--launcher-path",
        context
            .storage_dir
            .join("launchers")
            .join("01ARZ3NDEKTSV4RRFFQ69G5FB4.json")
            .to_str()
            .unwrap(),
    ]);
    cmd.timeout(std::time::Duration::from_secs(10));
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: GITHUB_APP_PRIVATE_KEY is not valid PEM or base64: Invalid symbol 37, offset 0.
    ");
}

// Bug 3: attach loop must leave interview_request.json in place until the
// engine consumes interview_response.json, so reattach remains safe.
#[test]
fn bug3_attach_leaves_interview_request_until_engine_consumes_response() {
    let context = test_context!();

    let run_dir = setup_run_dir(
        &context.storage_dir,
        "01ARZ3NDEKTSV4RRFFQ69G5FB5",
        serde_json::json!({}),
        &[
            r#"{"ts":"2026-01-01T00:00:01Z","run_id":"01ARZ3NDEKTSV4RRFFQ69G5FB5","event":"StageStarted","node_id":"gate","name":"Gate","index":0,"attempt":1,"max_attempts":1}"#,
        ],
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

    let mut cmd = context.command();
    cmd.args(["attach", "01ARZ3NDEKTSV4RRFFQ69G5FB5"]);
    cmd.write_stdin("y\n");
    cmd.timeout(std::time::Duration::from_secs(5));
    let _ = cmd.output();

    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "request_exists": runtime_state.interview_request_path().exists(),
            "response_exists": runtime_state.interview_response_path().exists(),
            "response": read_json(runtime_state.interview_response_path()),
        }),
        @r#"
        {
          "request_exists": true,
          "response_exists": true,
          "response": {
            "value": "Yes",
            "selected_option": null,
            "text": null
          }
        }
        "#
    );
}

#[test]
fn attach_closed_stdin_keeps_interview_pending() {
    let context = test_context!();

    let run_dir = setup_run_dir(
        &context.storage_dir,
        "01ARZ3NDEKTSV4RRFFQ69G5FB6",
        serde_json::json!({}),
        &[
            r#"{"ts":"2026-01-01T00:00:01Z","run_id":"01ARZ3NDEKTSV4RRFFQ69G5FB6","event":"StageStarted","node_id":"gate","name":"Gate","index":0,"attempt":1,"max_attempts":1}"#,
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
    let runtime_state = RuntimeState::new(&run_dir);
    std::fs::create_dir_all(runtime_state.runtime_dir()).unwrap();
    std::fs::write(
        runtime_state.interview_request_path(),
        serde_json::to_string(&question).unwrap(),
    )
    .unwrap();

    let mut cmd = context.command();
    cmd.args(["attach", "01ARZ3NDEKTSV4RRFFQ69G5FB6"]);
    cmd.timeout(std::time::Duration::from_secs(5));
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    ? Approve?
    [Y/N]: Interview ended without an answer. The run is still waiting for input; reattach to answer it.
    ");

    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "request_exists": runtime_state.interview_request_path().exists(),
            "response_exists": runtime_state.interview_response_path().exists(),
            "claim_exists": runtime_state.interview_claim_path().exists(),
        }),
        @r#"
        {
          "request_exists": true,
          "response_exists": false,
          "claim_exists": false
        }
        "#
    );
}

// Bug 4: attach should respect the verbose flag from run.json.
// Currently ProgressUI is created with verbose=false regardless of config.
#[test]
fn bug4_attach_respects_verbose_from_spec() {
    let context = test_context!();

    // Use pre-rename field names so handle_json_line can parse them
    // (isolates this test from bug 1). With 2 turns and 1 tool call,
    // verbose mode should display "(2 turns, 1 tools, ...)" in the output.
    let run_dir = setup_run_dir(
        &context.storage_dir,
        "01ARZ3NDEKTSV4RRFFQ69G5FB7",
        serde_json::json!({"verbose": true}),
        &[
            r#"{"ts":"2026-01-01T12:00:00Z","run_id":"01ARZ3NDEKTSV4RRFFQ69G5FB7","event":"StageStarted","node_id":"code","name":"Code","index":0,"attempt":1,"max_attempts":1}"#,
            r#"{"ts":"2026-01-01T12:00:01Z","run_id":"01ARZ3NDEKTSV4RRFFQ69G5FB7","event":"Agent.AssistantMessage","stage":"code","model":"claude-sonnet"}"#,
            r#"{"ts":"2026-01-01T12:00:02Z","run_id":"01ARZ3NDEKTSV4RRFFQ69G5FB7","event":"Agent.AssistantMessage","stage":"code","model":"claude-sonnet"}"#,
            r#"{"ts":"2026-01-01T12:00:03Z","run_id":"01ARZ3NDEKTSV4RRFFQ69G5FB7","event":"Agent.ToolCallStarted","stage":"code","tool_name":"read_file","tool_call_id":"tc1","arguments":{}}"#,
            r#"{"ts":"2026-01-01T12:00:04Z","run_id":"01ARZ3NDEKTSV4RRFFQ69G5FB7","event":"Agent.ToolCallCompleted","stage":"code","tool_name":"read_file","tool_call_id":"tc1","is_error":false}"#,
            r#"{"ts":"2026-01-01T12:00:10Z","run_id":"01ARZ3NDEKTSV4RRFFQ69G5FB7","event":"StageCompleted","node_id":"code","name":"Code","index":0,"duration_ms":10000,"status":"success","usage":{"input_tokens":1000,"output_tokens":500}}"#,
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

    let mut cmd = context.command();
    cmd.args(["attach", "01ARZ3NDEKTSV4RRFFQ69G5FB7"]);
    cmd.timeout(std::time::Duration::from_secs(10));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
        ✓ ?  10s  (0 turns, 0 tools, 1.5k toks)
    ");
}
