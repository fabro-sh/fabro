use fabro_test::{fabro_snapshot, test_context};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["llm", "prompt", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Execute a prompt

    Usage: fabro llm prompt [OPTIONS] [PROMPT]

    Arguments:
      [PROMPT]  The prompt text (also accepts stdin)

    Options:
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
      -m, --model <MODEL>              Model to use
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
      -s, --system <SYSTEM>            System prompt
          --no-stream                  Do not stream output
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
      -u, --usage                      Show token usage
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -S, --schema <SCHEMA>            JSON schema for structured output (inline JSON string)
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -o, --option <OPTION>            key=value options (temperature, `max_tokens`, `top_p`)
      -h, --help                       Print help
    ----- stderr -----
    ");
}
