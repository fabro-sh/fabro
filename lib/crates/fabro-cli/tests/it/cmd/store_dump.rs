use super::support::setup_completed_dry_run;
use insta::assert_snapshot;
use std::fs;
use std::time::Duration;

use crate::support::unique_run_id;
use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["store", "dump", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Export a run's durable state to a directory

    Usage: fabro store dump [OPTIONS] --output <OUTPUT> <RUN>

    Arguments:
      <RUN>  Run ID prefix or workflow name

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro/storage) [env: FABRO_STORAGE_DIR=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
      -o, --output <OUTPUT>            Output directory (must not exist or be empty)
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn store_dump_exports_large_command_output_backed_by_blob_refs() {
    let context = test_context!();
    let workflow = context.temp_dir.join("large-output.fabro");
    fs::write(
        &workflow,
        r#"digraph LargeOutput {
    graph [goal="Generate oversized command output"]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]
    big   [shape=parallelogram, label="Big", script="printf '%*s' 120000 '' | tr ' ' x"]

    start -> big -> exit
}
"#,
    )
    .unwrap();

    let run_id = unique_run_id();
    let mut run_cmd = context.run_cmd();
    run_cmd.current_dir(&context.temp_dir);
    run_cmd.timeout(Duration::from_secs(30));
    run_cmd.args([
        "--run-id",
        run_id.as_str(),
        "--no-retro",
        "--sandbox",
        "local",
    ]);
    run_cmd.arg(&workflow);
    let run_output = run_cmd.output().expect("command should execute");
    assert!(
        run_output.status.success(),
        "workflow run failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );

    let mut inspect_cmd = context.command();
    inspect_cmd.args(["inspect", "--json", &run_id]);
    let inspect_output = inspect_cmd.output().expect("inspect should execute");
    assert!(
        inspect_output.status.success(),
        "inspect failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&inspect_output.stdout),
        String::from_utf8_lossy(&inspect_output.stderr)
    );
    let inspect_json = String::from_utf8(inspect_output.stdout).unwrap();
    assert!(
        inspect_json.contains("blob://sha256/"),
        "inspect output should contain blob refs to exercise hydration\n{inspect_json}"
    );

    let output_dir = context.temp_dir.join("export");
    let mut dump_cmd = context.command();
    dump_cmd.args([
        "store",
        "dump",
        "--output",
        output_dir.to_str().unwrap(),
        &run_id,
    ]);
    let dump_output = dump_cmd.output().expect("store dump should execute");
    assert!(
        dump_output.status.success(),
        "store dump failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&dump_output.stdout),
        String::from_utf8_lossy(&dump_output.stderr)
    );

    let checkpoint = fs::read_to_string(output_dir.join("checkpoint.json")).unwrap();
    assert!(
        !checkpoint.contains("blob://sha256/"),
        "checkpoint export should hydrate blob refs\n{checkpoint}"
    );
}

#[test]
fn store_dump_exports_blob_refs_and_artifacts_together() {
    let context = test_context!();
    let workspace_dir = context.temp_dir.join("mixed-export");
    fs::create_dir_all(&workspace_dir).unwrap();

    fs::write(
        workspace_dir.join("mixed-export.fabro"),
        r#"digraph MixedExport {
    graph [goal="Generate oversized command output and artifacts"]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]
    big   [shape=parallelogram, label="Big", script="mkdir -p assets/shared && printf exported > assets/shared/report.txt && printf '%*s' 120000 '' | tr ' ' x"]

    start -> big -> exit
}
"#,
    )
    .unwrap();
    fs::write(
        workspace_dir.join("run.toml"),
        r#"version = 1
graph = "mixed-export.fabro"
goal = "Generate oversized command output and artifacts"

[sandbox]
provider = "local"
preserve = true

[sandbox.local]
worktree_mode = "never"

[artifacts]
include = ["assets/**"]
"#,
    )
    .unwrap();

    let run_id = unique_run_id();
    let mut run_cmd = context.run_cmd();
    run_cmd.current_dir(&workspace_dir);
    run_cmd.timeout(Duration::from_secs(30));
    run_cmd.args([
        "--run-id",
        run_id.as_str(),
        "--no-retro",
        "--sandbox",
        "local",
        "run.toml",
    ]);
    let run_output = run_cmd.output().expect("command should execute");
    assert!(
        run_output.status.success(),
        "workflow run failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );

    let mut inspect_cmd = context.command();
    inspect_cmd.args(["inspect", "--json", &run_id]);
    let inspect_output = inspect_cmd.output().expect("inspect should execute");
    assert!(
        inspect_output.status.success(),
        "inspect failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&inspect_output.stdout),
        String::from_utf8_lossy(&inspect_output.stderr)
    );
    let inspect_json = String::from_utf8(inspect_output.stdout).unwrap();
    assert!(
        inspect_json.contains("blob://sha256/"),
        "inspect output should contain blob refs to exercise hydration\n{inspect_json}"
    );

    let output_dir = context.temp_dir.join("export-mixed");
    let mut dump_cmd = context.command();
    dump_cmd.args([
        "store",
        "dump",
        "--output",
        output_dir.to_str().unwrap(),
        &run_id,
    ]);
    let dump_output = dump_cmd.output().expect("store dump should execute");
    assert!(
        dump_output.status.success(),
        "store dump failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&dump_output.stdout),
        String::from_utf8_lossy(&dump_output.stderr)
    );

    let checkpoint = fs::read_to_string(output_dir.join("checkpoint.json")).unwrap();
    assert!(
        !checkpoint.contains("blob://sha256/"),
        "checkpoint export should hydrate blob refs\n{checkpoint}"
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("artifacts/nodes/big/visit-1/assets/shared/report.txt"))
            .unwrap(),
        "exported"
    );
}

#[test]
fn store_dump_exports_completed_run_snapshot() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);
    let output_dir = context.temp_dir.join("export");

    let mut cmd = context.command();
    cmd.args([
        "store",
        "dump",
        "--output",
        output_dir.to_str().unwrap(),
        &run.run_id,
    ]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Exported 17 files for run [ULID] to [TEMP_DIR]/export
    ----- stderr -----
    ");

    assert_snapshot!(dump_file_summary(&output_dir), @"
    checkpoint.json
    checkpoints/0012.json
    checkpoints/0016.json
    checkpoints/0020.json
    conclusion.json
    events.jsonl
    graph.fabro
    nodes/exit/visit-1/status.json
    nodes/report/visit-1/response.md
    nodes/report/visit-1/status.json
    nodes/run_tests/visit-1/response.md
    nodes/run_tests/visit-1/status.json
    nodes/start/visit-1/status.json
    run.json
    sandbox.json
    start.json
    status.json
    ");
}

#[test]
fn store_dump_rejects_non_empty_output_dir() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);
    let output_dir = context.temp_dir.join("nonempty");
    std::fs::create_dir_all(&output_dir).unwrap();
    std::fs::write(output_dir.join("file.txt"), "x").unwrap();

    let mut cmd = context.command();
    cmd.args([
        "store",
        "dump",
        "--output",
        output_dir.to_str().unwrap(),
        &run.run_id,
    ]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: output path [TEMP_DIR]/nonempty already exists and is not an empty directory; remove it first or choose a different path
    ");
}

fn dump_file_summary(output_dir: &std::path::Path) -> String {
    let mut files: Vec<String> = walkdir::WalkDir::new(output_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            entry
                .path()
                .strip_prefix(output_dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    files.sort();
    files.join("\n") + "\n"
}
