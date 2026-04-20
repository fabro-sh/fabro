#![allow(
    clippy::absolute_paths,
    clippy::single_char_pattern,
    reason = "These secret-command tests use terse fixture patterns and explicit paths."
)]

use fabro_test::{fabro_snapshot, test_context};
use predicates::prelude::*;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.secret();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Manage server-owned secrets

    Usage: fabro secret [OPTIONS] <COMMAND>

    Commands:
      list  List secret names
      rm    Remove a secret
      set   Set a secret value
      help  Print this message or the help of the given subcommand(s)

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn test_secret_lifecycle() {
    let context = test_context!();

    let secret =
        |args: &[&str]| -> assert_cmd::assert::Assert { context.secret().args(args).assert() };

    // 1. set FOO=bar
    secret(&["set", "FOO", "bar"]).success();

    // 2. list -> contains FOO
    secret(&["list"])
        .success()
        .stdout(predicates::str::contains("FOO"))
        .stdout(predicates::str::contains("environment"));

    // 3. update FOO
    secret(&["set", "FOO", "updated"]).success();

    // 4. rm FOO
    secret(&["rm", "FOO"]).success();

    // 5. list no longer contains FOO
    let output = secret(&["list"]).success().get_output().stdout.clone();
    let stdout = String::from_utf8(output).unwrap();
    assert!(!stdout.contains("FOO"));
}

#[test]
fn test_secret_list_is_write_only() {
    let context = test_context!();

    let secret =
        |args: &[&str]| -> assert_cmd::assert::Assert { context.secret().args(args).assert() };

    secret(&["set", "A", "alpha-secret"]).success();
    secret(&["set", "B", "beta-secret"]).success();

    let out = secret(&["list"]).success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("A"));
    assert!(stdout.contains("B"));
    assert!(!stdout.contains("alpha-secret"));
    assert!(!stdout.contains("beta-secret"));
}

#[test]
fn test_secret_list_alias_ls() {
    let context = test_context!();

    context.secret().args(["set", "X", "y"]).assert().success();

    context
        .secret()
        .args(["ls"])
        .assert()
        .success()
        .stdout(predicates::str::contains("X"))
        .stdout(predicates::str::contains("environment"));
}

#[test]
fn test_secret_rm_missing_key() {
    let context = test_context!();
    let mut cmd = context.secret();
    cmd.args(["rm", "NOPE"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: secret not found: NOPE
    ");
}

#[test]
fn test_secret_value_with_equals() {
    let context = test_context!();

    context
        .secret()
        .args(["set", "URL", "https://x.com?a=1&b=2"])
        .assert()
        .success();

    context
        .secret()
        .args(["list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("URL"))
        .stdout(predicates::str::contains("environment"))
        .stdout(predicates::str::contains("https://x.com?a=1&b=2").not());
}

#[test]
fn test_file_secret_lifecycle() {
    let context = test_context!();

    let secret =
        |args: &[&str]| -> assert_cmd::assert::Assert { context.secret().args(args).assert() };

    secret(&[
        "set",
        "/tmp/test.pem",
        "pem-data",
        "--type",
        "file",
        "--description",
        "Test certificate",
    ])
    .success();

    secret(&["list"])
        .success()
        .stdout(predicates::str::contains("/tmp/test.pem"))
        .stdout(predicates::str::contains("file"))
        .stdout(predicates::str::contains("pem-data").not());

    secret(&["rm", "/tmp/test.pem"]).success();

    secret(&["list"])
        .success()
        .stdout(predicates::str::contains("/tmp/test.pem").not());
}
