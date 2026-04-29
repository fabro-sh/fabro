#![expect(
    clippy::disallowed_methods,
    reason = "integration tests stage fixtures with sync std::fs; test infrastructure, not Tokio-hot path"
)]

use fabro_test::{fabro_snapshot, test_context};

use super::support::{read_text, setup_local_sandbox_run, setup_seeded_created_dry_run, text_tree};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["sandbox", "cp", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Copy files to/from a run's sandbox

    Usage: fabro sandbox cp [OPTIONS] <SRC> <DST>

    Arguments:
      <SRC>  Source: <run-id>:<path> or local path
      <DST>  Destination: <run-id>:<path> or local path

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
      -r, --recursive         Recurse into directories
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn sandbox_cp_run_without_sandbox_json_errors_cleanly() {
    let context = test_context!();
    let run = setup_seeded_created_dry_run(&context);
    let dest = context.temp_dir.join("missing.txt");
    let mut cmd = context.cp();
    cmd.args([&format!("{}:foo.txt", run.run_id), dest.to_str().unwrap()]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × Run has no active sandbox.
    ");
}

#[test]
fn sandbox_cp_downloads_file_from_run() {
    let context = test_context!();
    let setup = setup_local_sandbox_run(&context);
    let dest = context.temp_dir.join("downloaded-root.txt");
    let mut cmd = context.cp();
    cmd.args([
        &format!("{}:sandbox_dir/download_me/root.txt", setup.run.run_id),
        dest.to_str().unwrap(),
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    ");
    assert_eq!(read_text(&dest), "keep");
}

#[test]
fn sandbox_cp_downloads_file_from_store_without_sandbox_json() {
    let context = test_context!();
    let setup = setup_local_sandbox_run(&context);
    let dest = context.temp_dir.join("downloaded-from-store.txt");
    let mut cmd = context.cp();
    cmd.args([
        &format!("{}:sandbox_dir/download_me/root.txt", setup.run.run_id),
        dest.to_str().unwrap(),
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    ");
    assert_eq!(read_text(&dest), "keep");
}

#[test]
fn sandbox_cp_uploads_file_to_run() {
    let context = test_context!();
    let setup = setup_local_sandbox_run(&context);
    let local = context.temp_dir.join("upload.txt");
    std::fs::write(&local, "uploaded-root")
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", local.display()));
    let mut cmd = context.cp();
    cmd.args([
        local.to_str().unwrap(),
        &format!("{}:sandbox_dir/uploaded.txt", setup.run.run_id),
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    ");
    assert_eq!(
        read_text(&setup.workspace_dir.join("sandbox_dir/uploaded.txt")),
        "uploaded-root"
    );
}

#[test]
fn sandbox_cp_recursive_downloads_directory() {
    let context = test_context!();
    let setup = setup_local_sandbox_run(&context);
    let dest = context.temp_dir.join("download-dir");
    let mut cmd = context.cp();
    cmd.args([
        "-r",
        &format!("{}:sandbox_dir/download_me", setup.run.run_id),
        dest.to_str().unwrap(),
    ]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    ");
    insta::assert_snapshot!(
        text_tree(&dest).join("\n"),
        @r"
        nested/child.txt = nested
        root.txt = keep
        "
    );
}
