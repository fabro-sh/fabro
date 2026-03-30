use insta::assert_snapshot;
use serde_json::json;

use fabro_test::{fabro_snapshot, test_context};

use super::support::{fixture, output_stdout, read_json, resolve_run};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["create", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Create a workflow run (allocate run dir, persist spec)

    Usage: fabro create [OPTIONS] <WORKFLOW>

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
fn create_persists_requested_overrides_into_run_json() {
    let context = test_context!();
    let workflow = fixture("simple.fabro");
    let mut cmd = context.command();
    cmd.args([
        "create",
        "--dry-run",
        "--auto-approve",
        "--goal",
        "Ship the release",
        "--model",
        "gpt-5",
        "--provider",
        "openai",
        "--sandbox",
        "local",
        "--label",
        "env=dev",
        "--label",
        "team=cli",
        "--verbose",
        "--no-retro",
        "--preserve-sandbox",
        workflow.to_str().unwrap(),
    ]);
    let output = cmd.output().expect("command should execute");
    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = output_stdout(&output);
    let run_id = stdout
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .expect("create should print a run ID")
        .to_string();
    let run = resolve_run(&context, &run_id);
    let run_json = read_json(&run.run_dir.join("run.json"));
    let labels = json!({
        "env": run_json.pointer("/labels/env"),
        "team": run_json.pointer("/labels/team"),
    });
    let compact = json!({
        "workflow_slug": run_json["workflow_slug"],
        "settings": {
            "goal": run_json.pointer("/settings/goal"),
            "dry_run": run_json.pointer("/settings/dry_run"),
            "auto_approve": run_json.pointer("/settings/auto_approve"),
            "no_retro": run_json.pointer("/settings/no_retro"),
            "verbose": run_json.pointer("/settings/verbose"),
            "llm": {
                "model": run_json.pointer("/settings/llm/model"),
                "provider": run_json.pointer("/settings/llm/provider"),
            },
            "sandbox": {
                "provider": run_json.pointer("/settings/sandbox/provider"),
                "preserve": run_json.pointer("/settings/sandbox/preserve"),
            },
        },
        "labels": labels,
    });

    assert_snapshot!(serde_json::to_string_pretty(&compact).unwrap(), @r###"
    {
      "workflow_slug": "simple",
      "settings": {
        "goal": "Ship the release",
        "dry_run": true,
        "auto_approve": true,
        "no_retro": true,
        "verbose": true,
        "llm": {
          "model": "gpt-5",
          "provider": "openai"
        },
        "sandbox": {
          "provider": "local",
          "preserve": true
        }
      },
      "labels": {
        "env": "dev",
        "team": "cli"
      }
    }
    "###);
}

#[test]
fn create_invalid_workflow_fails_without_creating_run() {
    let context = test_context!();
    let workflow = fixture("invalid.fabro");
    let mut cmd = context.command();
    cmd.args(["create", workflow.to_str().unwrap()]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Validation failed
    ");

    let runs_dir = context.storage_dir.join("runs");
    let run_count = std::fs::read_dir(&runs_dir)
        .ok()
        .map(|entries| entries.flatten().count())
        .unwrap_or(0);
    assert_eq!(run_count, 0, "invalid create should not persist a run");
}
