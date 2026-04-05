use fabro_test::{fabro_snapshot, test_context};

use super::support::run_state;
use crate::support::{fabro_json_snapshot, unique_run_id};

const SHARED_DAEMON_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["__detached", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Internal: run the engine process

    Usage: fabro __detached [OPTIONS] --run-id <RUN_ID> --run-dir <RUN_DIR> --launcher-path <LAUNCHER_PATH>

    Options:
          --json                           Output as JSON [env: FABRO_JSON=]
          --run-id <RUN_ID>                Run ID
          --debug                          Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --run-dir <RUN_DIR>              Run directory
          --launcher-path <LAUNCHER_PATH>  Launcher metadata path
          --no-upgrade-check               Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                          Suppress non-essential output [env: FABRO_QUIET=]
          --resume                         Resume from checkpoint instead of fresh start
          --verbose                        Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>      Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --server-url <SERVER_URL>        Server URL (overrides server.base_url from user.toml) [env: FABRO_SERVER_URL=]
      -h, --help                           Print help
    ----- stderr -----
    ");
}

fn launcher_path(context: &fabro_test::TestContext, run_id: &str) -> std::path::PathBuf {
    context
        .storage_dir
        .join("launchers")
        .join(format!("{run_id}.json"))
}

#[test]
fn detached_uses_cached_graph_after_source_deleted() {
    let context = test_context!();
    let run_id = unique_run_id();
    let workflow_path = context.temp_dir.join("workflow.fabro");

    context.write_temp(
        "workflow.fabro",
        "\
digraph CachedGraph {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    );

    context
        .command()
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

    let run_dir = context.find_run_dir(&run_id);
    std::fs::remove_file(&workflow_path).unwrap();

    context
        .command()
        .args([
            "__detached",
            "--run-id",
            run_id.as_str(),
            "--run-dir",
            run_dir.to_str().unwrap(),
            "--launcher-path",
            launcher_path(&context, &run_id).to_str().unwrap(),
        ])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .assert()
        .success();

    let conclusion = serde_json::to_value(
        run_state(&run_dir)
            .conclusion
            .expect("conclusion should exist"),
    )
    .unwrap();
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "status": conclusion["status"],
        }),
        @r#"
        {
          "status": "success"
        }
        "#
    );
}

#[test]
fn detached_uses_snapshotted_app_id_for_github_credentials() {
    let context = test_context!();
    let run_id = unique_run_id();
    let workflow_path = context.temp_dir.join("workflow.fabro");

    context.write_home(
        ".fabro/user.toml",
        "\
version = 1

[git]
app_id = \"snapshotted-app-id\"
",
    );
    context.write_temp(
        "workflow.fabro",
        "\
digraph GitHubApp {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    );

    context
        .command()
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

    let run_dir = context.find_run_dir(&run_id);
    let state = run_state(&run_dir);
    let run = state.run.as_ref().expect("run record should exist");
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "app_id": run.settings.git.clone().and_then(|git| git.app_id),
        }),
        @r#"
        {
          "app_id": "snapshotted-app-id"
        }
        "#
    );

    context.write_home(".fabro/user.toml", "version = 1\n");

    let mut cmd = context.command();
    cmd.env("GITHUB_APP_PRIVATE_KEY", "%%%not-base64%%%");
    cmd.args([
        "__detached",
        "--run-id",
        run_id.as_str(),
        "--run-dir",
        run_dir.to_str().unwrap(),
        "--launcher-path",
        launcher_path(&context, &run_id).to_str().unwrap(),
    ]);
    cmd.timeout(SHARED_DAEMON_TIMEOUT);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    ");
}

#[test]
fn detached_runs_without_run_json_when_run_id_is_explicit() {
    let context = test_context!();
    let run_id = unique_run_id();
    let workflow_path = context.temp_dir.join("workflow.fabro");

    context.write_temp(
        "workflow.fabro",
        "\
digraph DetachedStoreOnly {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    );

    context
        .command()
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

    let run_dir = context.find_run_dir(&run_id);
    context
        .command()
        .args([
            "__detached",
            "--run-id",
            run_id.as_str(),
            "--run-dir",
            run_dir.to_str().unwrap(),
            "--launcher-path",
            launcher_path(&context, &run_id).to_str().unwrap(),
        ])
        .timeout(SHARED_DAEMON_TIMEOUT)
        .assert()
        .success();

    let conclusion = serde_json::to_value(
        run_state(&run_dir)
            .conclusion
            .expect("conclusion should exist"),
    )
    .unwrap();
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "status": conclusion["status"],
        }),
        @r#"
        {
          "status": "success"
        }
        "#
    );
}

#[test]
fn detached_resume_rejects_completed_run_without_mutating_it() {
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
        .timeout(SHARED_DAEMON_TIMEOUT)
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
      "run_dir": "[RUN_DIR]",
      "start_time": "[TIMESTAMP]",
      "conclusion_timestamp": "[TIMESTAMP]",
      "conclusion_status": "success"
    }
    "#);

    let mut cmd = context.command();
    cmd.args([
        "__detached",
        "--run-id",
        &run_id,
        "--run-dir",
        &run_dir,
        "--launcher-path",
        launcher_path(&context, &run_id).to_str().unwrap(),
        "--resume",
    ]);
    cmd.timeout(SHARED_DAEMON_TIMEOUT);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
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
