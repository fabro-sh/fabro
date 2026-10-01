use std::fmt::Write as _;

use fabro_test::{TestContext, test_context};
use httpmock::MockServer;
use serde_json::json;

/// Exercise admission and the real agent against two local providers. Checking
/// the request path and wire model catches overrides that only reach run state.
fn assert_agent_route(
    mut context: TestContext,
    flags: &[&str],
    graph_defaults: &str,
    node_settings: &str,
    expected_provider: &str,
    expected_model: &str,
) {
    let endpoint = MockServer::start();
    let mut settings = "_version = 1\n[server.auth]\nmethods = [\"dev-token\"]\n".to_string();
    for provider in ["primary", "alternate"] {
        write!(
            settings,
            r#"
[llm.providers.{provider}]
display_name = "{provider}"
base_url = "{base}/{provider}/v1"
auth = {{ type = "none" }}
default_model = "base"
[llm.providers.{provider}.metadata.agent]
profile = "openai"
"#,
            base = endpoint.base_url(),
        )
        .expect("provider settings should format");
        for model in ["base", "override", "node"] {
            write!(
                settings,
                r#"
[llm.providers.{provider}.models.{model}]
display_name = "{model}"
api_model = "{provider}-{model}-wire"
limits = {{ context_tokens = 32000, max_output_tokens = 1000 }}
capabilities = {{ text = true, tools = true }}
"#,
            )
            .expect("model settings should format");
        }
    }
    context.write_home(".fabro/settings.toml", settings);
    context.isolated_server();
    context.write_temp(
        "workflow.toml",
        r#"_version = 1
[workflow]
graph = "workflow.fabro"
[run.model]
provider = "primary"
name = "base"
[run.pull_request]
enabled = false
"#,
    );
    context.write_temp(
        "workflow.fabro",
        format!(
            r#"digraph ModelSelection {{
  {graph_defaults}
  start [shape=Mdiamond];
  work [shape=box, prompt="Say hello."];
  {node_settings}
  exit [shape=Msquare];
  start -> work -> exit;
}}"#,
        ),
    );

    let wire_model = format!("{expected_provider}-{expected_model}-wire");
    let chunk = |delta, finish_reason| {
        json!({
            "id": "scripted-response", "object": "chat.completion.chunk",
            "created": 1, "model": wire_model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}]
        })
    };
    let response = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        chunk(
            json!({"role": "assistant", "content": "Hello."}),
            json!(null)
        ),
        chunk(json!({}), json!("stop")),
    );
    let expected = endpoint.mock(|when, then| {
        when.method("POST")
            .path(format!("/{expected_provider}/v1/chat/completions"))
            .json_body_includes(json!({"model": wire_model, "stream": true}).to_string());
        then.status(200)
            .header("Content-Type", "text/event-stream")
            .body(&response);
    });
    // Respond immediately to a wrong route too, so the regression fails with
    // the mismatched request instead of waiting through model retry backoff.
    let unexpected = endpoint.mock(|when, then| {
        when.method("POST");
        then.status(200)
            .header("Content-Type", "text/event-stream")
            .body(&response);
    });

    let output = context
        .run_cmd()
        .args(["--auto-approve", "--environment", "local"])
        .args(flags)
        .arg(context.temp_dir.join("workflow.toml"))
        .output()
        .expect("run should execute");
    assert!(
        output.status.success(),
        "run failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    unexpected.assert_calls(0);
    assert!(
        expected.calls() > 0,
        "the agent must call the requested route"
    );
}

#[test]
fn workflow_model_controls_agent_requests() {
    assert_agent_route(test_context!(), &[], "", "", "primary", "base");
}

#[test]
fn model_flag_overrides_workflow_model_at_execution() {
    assert_agent_route(
        test_context!(),
        &["--model", "override"],
        "",
        "",
        "primary",
        "override",
    );
}

#[test]
fn provider_flag_overrides_workflow_provider_at_execution() {
    assert_agent_route(
        test_context!(),
        &["--provider", "alternate"],
        "",
        "",
        "alternate",
        "base",
    );
}

#[test]
fn model_flags_override_workflow_and_graph_defaults_at_execution() {
    assert_agent_route(
        test_context!(),
        &["--model", "override", "--provider", "alternate"],
        r#"graph [default_model="base", default_provider="primary"];"#,
        "",
        "alternate",
        "override",
    );
}

#[test]
fn explicit_node_model_takes_precedence_over_flags() {
    assert_agent_route(
        test_context!(),
        &["--model", "override", "--provider", "alternate"],
        "",
        r#"work [model="node", provider="primary"];"#,
        "primary",
        "node",
    );
}

#[test]
fn node_stylesheet_model_takes_precedence_over_flags() {
    assert_agent_route(
        test_context!(),
        &["--model", "override", "--provider", "alternate"],
        "",
        r##"graph [model_stylesheet="#work { model: node; provider: primary; }"];"##,
        "primary",
        "node",
    );
}
