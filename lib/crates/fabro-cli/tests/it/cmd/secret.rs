use fabro_test::{fabro_snapshot, test_context};
use predicates;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.secret();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Manage secrets in ~/.fabro/.env

    Usage: fabro secret [OPTIONS] <COMMAND>

    Commands:
      get   Get a secret value
      list  List secret names
      rm    Remove a secret
      set   Set a secret value
      help  Print this message or the help of the given subcommand(s)

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
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
fn test_secret_lifecycle() {
    let context = test_context!();

    let secret =
        |args: &[&str]| -> assert_cmd::assert::Assert { context.secret().args(args).assert() };

    // 1. set FOO=bar
    secret(&["set", "FOO", "bar"]).success();

    // 2. get FOO -> stdout is "bar\n"
    secret(&["get", "FOO"]).success().stdout("bar\n");

    // 3. list -> contains FOO
    secret(&["list"])
        .success()
        .stdout(predicates::str::contains("FOO"));

    // 4. update FOO
    secret(&["set", "FOO", "updated"]).success();

    // 5. get FOO -> "updated\n"
    secret(&["get", "FOO"]).success().stdout("updated\n");

    // 6. rm FOO
    secret(&["rm", "FOO"]).success();

    // 7. get FOO -> fails
    secret(&["get", "FOO"]).failure();
}

#[test]
fn test_secret_list_show_values() {
    let context = test_context!();

    let secret =
        |args: &[&str]| -> assert_cmd::assert::Assert { context.secret().args(args).assert() };

    secret(&["set", "A", "1"]).success();
    secret(&["set", "B", "2"]).success();

    // Without --show-values: just keys
    let out = secret(&["list"]).success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("A"));
    assert!(stdout.contains("B"));
    assert!(!stdout.contains("A=1"));

    // With --show-values: KEY=VALUE
    secret(&["list", "--show-values"])
        .success()
        .stdout(predicates::str::contains("A=1"))
        .stdout(predicates::str::contains("B=2"));
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
        .stdout(predicates::str::contains("X"));
}

#[test]
fn test_secret_get_missing_key() {
    let context = test_context!();
    let mut cmd = context.secret();
    cmd.args(["get", "NOPE"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: secret not found: NOPE
    ");
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
        .args(["get", "URL"])
        .assert()
        .success()
        .stdout("https://x.com?a=1&b=2\n");
}
