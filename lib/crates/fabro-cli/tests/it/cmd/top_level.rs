use fabro_test::{fabro_snapshot, test_context};

#[test]
fn version() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.arg("--version");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    fabro [VERSION] ([BUILD])
    ----- stderr -----
    ");
}

#[test]
fn no_dotenv_flag() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["--no-dotenv", "doctor"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: unexpected argument '--no-dotenv' found

    Usage: fabro [OPTIONS] <COMMAND>

    For more information, try '--help'.
    ");
}
