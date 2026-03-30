use fabro_test::{fabro_snapshot, test_context};

#[test]
fn prompt_bad_option() {
    let context = test_context!();
    let mut cmd = context.llm();
    cmd.args(["prompt", "-o", "bad_option", "hello"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: invalid value 'bad_option' for '--option <OPTION>': expected key=value, got bad_option

    For more information, try '--help'.
    ");
}

#[test]
fn prompt_no_text() {
    let context = test_context!();
    let mut cmd = context.llm();
    cmd.arg("prompt");
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    error: Error: no prompt provided. Pass a prompt as an argument or pipe text via stdin.
    ");
}

#[test]
fn prompt_schema_invalid() {
    let context = test_context!();
    let mut cmd = context.llm();
    cmd.args([
        "prompt",
        "--no-stream",
        "-m",
        "test-model",
        "--schema",
        "not json",
        "hello",
    ]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    Using model: test-model
    error: --schema must be valid JSON
      > expected ident at line 1 column 2
    ");
}
