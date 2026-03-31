use insta::assert_snapshot;

use fabro_test::{fabro_snapshot, test_context};

use super::support::setup_completed_dry_run;

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
      -o, --output <OUTPUT>            Output directory (must not exist or be empty)
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
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
    Exported 14 files for run [ULID] to [TEMP_DIR]/export
    ----- stderr -----
    ");

    assert_snapshot!(dump_file_summary(&output_dir), @r###"
    checkpoint.json
    checkpoints/0001.json
    checkpoints/0002.json
    checkpoints/0003.json
    conclusion.json
    events.jsonl
    graph.fabro
    nodes/report/visit-1/status.json
    nodes/run_tests/visit-1/status.json
    nodes/start/visit-1/status.json
    run.json
    sandbox.json
    start.json
    status.json
    "###);
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
