use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.variable();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Manage server-owned variables

    Usage: fabro variable [OPTIONS] <COMMAND>

    Commands:
      list  List variables
      get   Get a variable value
      rm    Remove a variable
      set   Set a variable value
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
fn variable_lifecycle() {
    let context = test_context!();
    let name = format!("DEPLOY_ENV_{}", context.test_case_id());

    context
        .variable()
        .args([
            "set",
            &name,
            "staging",
            "--description",
            "Deployment target",
        ])
        .assert()
        .success()
        .stderr(format!("Set {name}\n"));

    context
        .variable()
        .args(["list"])
        .assert()
        .success()
        .stdout(predicates::str::contains(&name))
        .stdout(predicates::str::contains("staging"))
        .stdout(predicates::str::contains("UPDATED"));

    context
        .variable()
        .args(["get", &name])
        .assert()
        .success()
        .stdout("staging\n");

    context
        .variable()
        .args(["set", &name, "production"])
        .assert()
        .success();

    context
        .variable()
        .args(["get", &name])
        .assert()
        .success()
        .stdout("production\n");

    context
        .variable()
        .args(["rm", &name])
        .assert()
        .success()
        .stderr(format!("Removed {name}\n"));

    context
        .variable()
        .args(["get", &name])
        .assert()
        .failure()
        .stderr(predicates::str::contains(format!(
            "variable not found: {name}"
        )));
}
