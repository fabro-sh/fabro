use fabro_test::{fabro_snapshot, test_context};

#[test]
fn invalid_permissions() {
    let context = test_context!();
    let mut cmd = context.exec_cmd();
    cmd.args(["--permissions", "bogus", "test prompt"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: invalid value 'bogus' for '--permissions <PERMISSIONS>'
      [possible values: read-only, read-write, full]

    For more information, try '--help'.
    ");
}

#[test]
fn no_prompt() {
    let context = test_context!();
    fabro_snapshot!(context.filters(), context.exec_cmd(), @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: the following required arguments were not provided:
      <PROMPT>

    Usage: fabro exec --no-upgrade-check --storage-dir <STORAGE_DIR> <PROMPT>

    For more information, try '--help'.
    ");
}
