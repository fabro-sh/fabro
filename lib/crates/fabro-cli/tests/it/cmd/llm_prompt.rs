use fabro_test::{fabro_snapshot, test_context};
#[cfg(feature = "server")]
use httpmock::prelude::*;
#[cfg(feature = "server")]
use serde_json::Value;

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
          --json                       Output as JSON [env: FABRO_JSON=]
      -m, --model <MODEL>              Model to use
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
      -s, --system <SYSTEM>            System prompt
          --no-stream                  Do not stream output
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
      -u, --usage                      Show token usage
      -S, --schema <SCHEMA>            JSON schema for structured output (inline JSON string)
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -o, --option <OPTION>            key=value options (temperature, `max_tokens`, `top_p`)
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
#[cfg(feature = "server")]
fn prompt_json_streaming_server_reports_resolved_model() {
    let context = test_context!();
    let server = MockServer::start();
    let sse_body = "\
event: stream_event\n\
data: {\"type\":\"text_delta\",\"delta\":\"Hi\",\"text_id\":null}\n\
\n\
event: stream_event\n\
data: {\"type\":\"finish\",\"finish_reason\":\"stop\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"total_tokens\":7},\"response\":{\"id\":\"r1\",\"model\":\"resolved-model\",\"provider\":\"test-provider\",\"message\":{\"role\":\"assistant\",\"content\":[{\"kind\":\"text\",\"data\":\"Hi\"}],\"name\":null,\"tool_call_id\":null},\"finish_reason\":\"stop\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"total_tokens\":7},\"raw\":null,\"warnings\":[],\"rate_limit\":null}}\n\
\n";

    let mock = server.mock(|when, then| {
        when.method(POST).path("/completions");
        then.status(200)
            .header("content-type", "text/event-stream")
            .body(sse_body);
    });

    let output = context
        .command()
        .env_remove("FABRO_STORAGE_DIR")
        .args([
            "--server-url",
            &server.url(""),
            "--json",
            "llm",
            "prompt",
            "Hello",
        ])
        .output()
        .expect("command should run");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let value: Value =
        serde_json::from_slice(&output.stdout).expect("llm prompt JSON should parse");
    assert_eq!(value["response"], "Hi");
    assert_eq!(value["model"], "resolved-model");
    assert_eq!(value["usage"]["input_tokens"], 5);
    assert_eq!(value["usage"]["output_tokens"], 2);
    mock.assert();
}
