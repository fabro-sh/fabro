#![expect(
    clippy::disallowed_methods,
    reason = "These CLI integration tests intentionally spawn the real fabro binary and stream DOT over stdio to verify the internal render subprocess contract."
)]
#![expect(
    clippy::disallowed_types,
    reason = "integration tests write DOT to the spawned child's stdin via std::io::Write"
)]

use std::io::Write;
use std::process::{Command, Stdio};

use fabro_test::{fabro_snapshot, test_context};

fn render_graph_command(context: &fabro_test::TestContext) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fabro"));
    fabro_test::apply_test_isolation(&mut cmd, &context.home_dir);
    cmd.current_dir(&context.temp_dir);
    cmd
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["__render-graph", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Render a DOT graph to SVG (internal)

    Usage: fabro __render-graph [OPTIONS]

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn render_graph_outputs_svg() {
    let context = test_context!();
    let mut cmd = render_graph_command(&context);
    cmd.args(["__render-graph"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("render-graph subprocess should spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin should be piped")
        .write_all(b"digraph { a -> b }")
        .expect("stdin write should succeed");

    let output = child
        .wait_with_output()
        .expect("render-graph subprocess should exit");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be valid UTF-8");
    assert!(
        stdout.contains("<svg"),
        "expected SVG output, got: {}",
        &stdout[..stdout.len().min(200)]
    );
}

#[test]
fn render_graph_bad_input_uses_render_error_protocol() {
    let context = test_context!();
    let mut cmd = render_graph_command(&context);
    cmd.args(["__render-graph"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("render-graph subprocess should spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin should be piped")
        .write_all(b"not valid dot")
        .expect("stdin write should succeed");

    let output = child
        .wait_with_output()
        .expect("render-graph subprocess should exit");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be valid UTF-8");
    assert!(
        stdout.starts_with("RENDER_ERROR:"),
        "expected render error protocol, got: {stdout}"
    );
}
