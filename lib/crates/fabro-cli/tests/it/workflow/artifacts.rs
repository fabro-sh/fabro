#![expect(
    clippy::disallowed_methods,
    reason = "integration test initializes an isolated git repository with the system git binary"
)]

use fabro_test::test_context;

use super::{find_run_dir, run_id_for};
use crate::cmd::support::output_stdout;

#[test]
fn unchanged_matching_artifact_is_captured_once_across_stages() {
    let mut context = test_context!();
    context.write_home(
        ".fabro/settings.toml",
        "[server.auth]\nmethods = [\"dev-token\"]\n",
    );
    context.isolated_server();
    context.write_temp(
        "artifact_dedupe.fabro",
        r#"digraph ArtifactDedupe {
  graph [goal="Deduplicate unchanged artifacts"]
  start [shape=Mdiamond]
  create [shape=parallelogram, script="mkdir -p assets && printf unchanged > assets/report.txt"]
  inspect [shape=parallelogram, script="test -f assets/report.txt"]
  exit [shape=Msquare]
  start -> create -> inspect -> exit
}
"#,
    );
    context.write_temp(
        "run.toml",
        r#"_version = 1

[workflow]
graph = "artifact_dedupe.fabro"

[run]
goal = "Deduplicate unchanged artifacts"

[run.checkpoint]
exclude_globs = ["assets/**"]

[run.artifacts]
include = ["assets/**"]
"#,
    );
    init_git_repo(&context.temp_dir);

    context
        .run_cmd()
        .args(["--auto-approve", "--sandbox", "local"])
        .arg(context.temp_dir.join("run.toml"))
        .assert()
        .success();

    let run_dir = find_run_dir(&context);
    let run_id = run_id_for(&run_dir);
    let output = context
        .command()
        .args(["--json", "artifact", "list", &run_id])
        .output()
        .expect("artifact list should execute");
    assert!(
        output.status.success(),
        "artifact list failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let artifacts: Vec<serde_json::Value> =
        serde_json::from_str(&output_stdout(&output)).expect("artifact list JSON should parse");
    let report_entries = artifacts
        .iter()
        .filter(|artifact| artifact["relative_path"] == "assets/report.txt")
        .count();

    assert_eq!(
        report_entries, 1,
        "unchanged artifact should be captured once: {artifacts:?}"
    );
}

fn init_git_repo(dir: &std::path::Path) {
    std::process::Command::new("git")
        .arg("init")
        .current_dir(dir)
        .output()
        .expect("git init should run");
    std::process::Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(dir)
        .output()
        .expect("git config user.name should run");
    std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(dir)
        .output()
        .expect("git config user.email should run");
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()
        .expect("git add should run");
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(dir)
        .output()
        .expect("git commit should run");
}
