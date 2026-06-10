//! Wire snapshots for the Anthropic Messages dialect.

use std::sync::Arc;

use fabro_llm::provider::ProviderAdapter;
use fabro_llm::providers::AnthropicAdapter;
use fabro_llm::types::{
    Message, ReasoningEffort, Request, ResponseFormat, ResponseFormatType, StreamEvent, ToolChoice,
    ToolDefinition,
};
use fabro_llm::{Error, ProviderErrorKind};
use fabro_model::Catalog;
use futures::StreamExt;
use httpmock::prelude::*;

use crate::support::{
    self, WireCapture, base_request, corpus_audio_attachment, corpus_bad_file_path_attachments,
    corpus_inline_attachments, corpus_multi_turn, corpus_provider_options, corpus_response_format,
    corpus_sampling_params, corpus_thinking_round_trip, corpus_tool_round_trip, corpus_tools,
    corpus_url_attachments, json_schema_format, mount_capture, mount_capture_sse, take_capture,
};

const MODEL: &str = "claude-sonnet-4-20250514";

/// Minimal valid Messages API body for encode-side tests that only assert on
/// the captured request.
fn minimal_body() -> serde_json::Value {
    serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": MODEL,
        "content": [{"type": "text", "text": "ok"}],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": 1, "output_tokens": 1}
    })
}

/// Runs `complete()` against a capture mock and returns the captured wire
/// request.
async fn encode_capture(adapter: AnthropicAdapter, request: &Request) -> WireCapture {
    let server = MockServer::start();
    let (mock, slot) = mount_capture(&server, "/messages", minimal_body());
    let adapter = adapter.with_base_url(server.base_url());
    adapter
        .complete(request)
        .await
        .expect("complete should succeed");
    mock.assert();
    take_capture(&slot)
}

/// Runs `stream()` against an SSE transcript and returns the captured wire
/// request plus every emitted stream item as JSON.
async fn stream_capture(
    adapter: AnthropicAdapter,
    request: &Request,
    sse_body: &str,
) -> (WireCapture, Vec<serde_json::Value>) {
    let server = MockServer::start();
    let (mock, slot) = mount_capture_sse(&server, "/messages", sse_body);
    let adapter = adapter.with_base_url(server.base_url());
    let events = support::collect_stream_events(&adapter, request).await;
    mock.assert();
    (take_capture(&slot), events)
}

fn adapter() -> AnthropicAdapter {
    AnthropicAdapter::new("test-key")
}

fn builtin_catalog() -> Arc<Catalog> {
    Arc::new(Catalog::from_builtin().expect("built-in catalog should build"))
}

fn header_value<'a>(capture: &'a WireCapture, name: &str) -> Option<&'a str> {
    capture
        .headers
        .iter()
        .find(|(header, _)| header == name)
        .map(|(_, value)| value.as_str())
}

// ---------------------------------------------------------------------------
// Round trip (encode + decode)
// ---------------------------------------------------------------------------

/// Shared setup for the system+tools round trip: runs `complete()` against a
/// canned response and returns both the captured request and decoded response
/// so the encode and decode halves can be pinned by separate tests.
async fn system_and_tools_roundtrip() -> (WireCapture, fabro_llm::types::Response) {
    let server = MockServer::start();
    let (mock, slot) = mount_capture(
        &server,
        "/messages",
        serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-20250514",
            "content": [{"type": "text", "text": "Hello back"}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 42,
                "output_tokens": 7,
                "cache_read_input_tokens": 10,
                "cache_creation_input_tokens": 3
            }
        }),
    );

    let adapter = AnthropicAdapter::new("test-key").with_base_url(server.base_url());
    let request = Request {
        messages: vec![Message::system("Be concise"), Message::user("Hello")],
        tools: Some(vec![ToolDefinition::function(
            "search",
            "Search files",
            serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}}),
        )]),
        temperature: Some(0.5),
        ..base_request("claude-sonnet-4-20250514")
    };

    let response = adapter
        .complete(&request)
        .await
        .expect("complete should succeed");
    mock.assert();
    (take_capture(&slot), response)
}

#[tokio::test]
async fn system_and_tools_encode() {
    let (capture, _) = system_and_tools_roundtrip().await;
    fabro_test::fabro_json_snapshot!(capture);
}

#[tokio::test]
async fn system_and_tools_decode() {
    let (_, response) = system_and_tools_roundtrip().await;
    fabro_test::fabro_json_snapshot!(response);
}

// ---------------------------------------------------------------------------
// Encode
// ---------------------------------------------------------------------------

#[tokio::test]
async fn encode_multi_turn() {
    let capture = encode_capture(adapter(), &corpus_multi_turn(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture);
}

#[tokio::test]
async fn encode_tool_choice_auto() {
    let capture = encode_capture(adapter(), &corpus_tools(MODEL, Some(ToolChoice::Auto))).await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_tool_choice_required() {
    let capture = encode_capture(adapter(), &corpus_tools(MODEL, Some(ToolChoice::Required))).await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_tool_choice_named() {
    let capture = encode_capture(
        adapter(),
        &corpus_tools(MODEL, Some(ToolChoice::named("search"))),
    )
    .await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_tool_choice_none() {
    let capture = encode_capture(adapter(), &corpus_tools(MODEL, Some(ToolChoice::None))).await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_tool_round_trip() {
    let capture = encode_capture(adapter(), &corpus_tool_round_trip(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_thinking_round_trip() {
    let capture = encode_capture(adapter(), &corpus_thinking_round_trip(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_inline_attachments() {
    let capture = encode_capture(adapter(), &corpus_inline_attachments(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_url_attachments() {
    let capture = encode_capture(adapter(), &corpus_url_attachments(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_bad_file_path_attachments_dropped() {
    let capture = encode_capture(adapter(), &corpus_bad_file_path_attachments(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_audio_attachment() {
    let capture = encode_capture(adapter(), &corpus_audio_attachment(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_response_format_json_object() {
    let format = ResponseFormat {
        kind:        ResponseFormatType::JsonObject,
        json_schema: None,
        strict:      false,
    };
    let capture = encode_capture(adapter(), &corpus_response_format(MODEL, format)).await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_response_format_json_schema() {
    let capture = encode_capture(
        adapter(),
        &corpus_response_format(MODEL, json_schema_format()),
    )
    .await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_sampling_params() {
    let capture = encode_capture(adapter(), &corpus_sampling_params(MODEL)).await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_provider_options_anthropic_namespace() {
    let capture = encode_capture(
        adapter(),
        &corpus_provider_options(MODEL, serde_json::json!({"anthropic": {"top_k": 5}})),
    )
    .await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_reasoning_effort_with_levels_catalog() {
    let catalog = support::catalog_from_toml(
        r#"
[providers.anthropic]
display_name = "Anthropic"
adapter = "anthropic"
agent_profile = "anthropic"

[models."test-claude"]
provider = "anthropic"
display_name = "Test Claude"
family = "claude"
default = true

[models."test-claude".limits]
context_window = 200000
max_output = 4096

[models."test-claude".features]
tools = true
vision = true
reasoning = true
reasoning_effort = "levels"
prompt_cache = false
"#,
    );
    let request = Request {
        reasoning_effort: Some(fabro_llm::types::ReasoningEffort::High),
        ..base_request("test-claude")
    };
    let capture = encode_capture(adapter().with_catalog(catalog), &request).await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_fable_uses_api_id_effort_and_omits_1m_beta() {
    let request = Request {
        reasoning_effort: Some(ReasoningEffort::XHigh),
        temperature: Some(0.0),
        top_p: Some(0.5),
        ..base_request("fable")
    };

    let capture = encode_capture(adapter().with_catalog(builtin_catalog()), &request).await;

    assert_eq!(capture.body["model"], "claude-fable-5");
    assert_eq!(capture.body["output_config"]["effort"], "xhigh");
    assert!(capture.body.get("thinking").is_none());
    assert!(capture.body.get("temperature").is_none());
    assert!(capture.body.get("top_p").is_none());
    assert!(
        !header_value(&capture, "anthropic-beta")
            .unwrap_or("")
            .contains("context-1m-2025-08-07"),
        "Fable has 1M context by default and must not receive the legacy beta header"
    );
}

#[tokio::test]
async fn encode_opus_omits_1m_beta_header() {
    let capture = encode_capture(
        adapter().with_catalog(builtin_catalog()),
        &base_request("claude-opus-4-8"),
    )
    .await;

    assert!(
        !header_value(&capture, "anthropic-beta")
            .unwrap_or("")
            .contains("context-1m-2025-08-07"),
        "1M context is GA on opus; the legacy beta opt-in must not be sent"
    );
}

#[tokio::test]
async fn encode_opus_drops_sampling_params() {
    let request = Request {
        temperature: Some(0.0),
        top_p: Some(0.5),
        ..base_request("claude-opus-4-8")
    };

    let capture = encode_capture(adapter().with_catalog(builtin_catalog()), &request).await;

    assert!(
        capture.body.get("temperature").is_none(),
        "Opus 4.7/4.8 reject temperature; it must not be sent"
    );
    assert!(
        capture.body.get("top_p").is_none(),
        "Opus 4.7/4.8 reject top_p; it must not be sent"
    );
}

#[tokio::test]
async fn encode_opus_effort_keeps_adaptive_thinking() {
    let request = Request {
        reasoning_effort: Some(ReasoningEffort::High),
        ..base_request("claude-opus-4-8")
    };

    let capture = encode_capture(adapter().with_catalog(builtin_catalog()), &request).await;

    assert_eq!(capture.body["output_config"]["effort"], "high");
    assert_eq!(
        capture.body["thinking"]["type"], "adaptive",
        "asking for effort must not turn thinking off; Opus 4.7/4.8 run without thinking unless adaptive is sent"
    );
}

#[tokio::test]
async fn encode_opus_without_effort_injects_adaptive_thinking() {
    let capture = encode_capture(
        adapter().with_catalog(builtin_catalog()),
        &base_request("claude-opus-4-8"),
    )
    .await;

    assert_eq!(capture.body["thinking"]["type"], "adaptive");
}

#[tokio::test]
async fn encode_fable_without_effort_omits_default_thinking() {
    let capture = encode_capture(
        adapter().with_catalog(builtin_catalog()),
        &base_request("claude-fable-5"),
    )
    .await;

    assert_eq!(capture.body["model"], "claude-fable-5");
    assert!(capture.body.get("thinking").is_none());
}

#[test]
fn fable_rejects_manual_enabled_or_disabled_thinking() {
    let adapter = adapter().with_catalog(builtin_catalog());

    for kind in ["enabled", "disabled"] {
        let request = Request {
            provider_options: Some(serde_json::json!({
                "anthropic": {
                    "thinking": {"type": kind, "budget_tokens": 1024}
                }
            })),
            ..base_request("claude-fable-5")
        };

        let err = adapter
            .validate_request(&request)
            .expect_err("manual Fable thinking mode should be rejected locally");
        assert!(
            err.to_string().contains("Claude Fable 5")
                && err.to_string().contains("thinking")
                && err.to_string().contains(kind),
            "unexpected error: {err}"
        );
    }
}

#[tokio::test]
async fn encode_prompt_cache_with_catalog() {
    let catalog = support::catalog_from_toml(
        r#"
[providers.anthropic]
display_name = "Anthropic"
adapter = "anthropic"
agent_profile = "anthropic"

[models."test-claude"]
provider = "anthropic"
display_name = "Test Claude"
family = "claude"
default = true

[models."test-claude".limits]
context_window = 200000
max_output = 4096

[models."test-claude".features]
tools = true
vision = true
reasoning = true
prompt_cache = true
"#,
    );
    let request = Request {
        messages: vec![
            Message::system("You are a careful reviewer."),
            Message::user("Review this."),
        ],
        ..corpus_tools("test-claude", None)
    };
    // Full capture: the prompt-cache path also controls the beta header.
    let capture = encode_capture(adapter().with_catalog(catalog), &request).await;
    fabro_test::fabro_json_snapshot!(capture);
}

#[tokio::test]
async fn count_tokens_wire_shape() {
    let server = MockServer::start();
    let (mock, slot) = mount_capture(
        &server,
        "/messages/count_tokens",
        serde_json::json!({"input_tokens": 123}),
    );

    let adapter = adapter().with_base_url(server.base_url());
    let request = Request {
        messages: vec![Message::system("Be concise"), Message::user("Hello")],
        ..corpus_tools(MODEL, None)
    };
    let count = adapter
        .count_input_tokens(&request)
        .await
        .unwrap()
        .expect("anthropic should count tokens");

    mock.assert();
    assert_eq!(count.input_tokens, 123);
    fabro_test::fabro_json_snapshot!(take_capture(&slot));
}

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

/// Runs `complete()` against a canned body and returns the decoded response.
async fn decode_response(body: serde_json::Value) -> fabro_llm::types::Response {
    let server = MockServer::start();
    let (mock, _slot) = mount_capture(&server, "/messages", body);
    let adapter = adapter().with_base_url(server.base_url());
    let response = adapter
        .complete(&base_request(MODEL))
        .await
        .expect("complete should succeed");
    mock.assert();
    response
}

/// Runs `complete()` against a canned body and returns the adapter result.
async fn complete_result(body: serde_json::Value) -> Result<fabro_llm::types::Response, Error> {
    let server = MockServer::start();
    let (mock, _slot) = mount_capture(&server, "/messages", body);
    let adapter = adapter().with_base_url(server.base_url());
    let result = adapter.complete(&base_request(MODEL)).await;
    mock.assert();
    result
}

#[tokio::test]
async fn decode_tool_use_stop_reason() {
    let response = decode_response(serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": MODEL,
        "content": [
            {"type": "text", "text": "Let me search."},
            {
                "type": "tool_use",
                "id": "toolu_01",
                "name": "search",
                "input": {"query": "foo"}
            }
        ],
        "stop_reason": "tool_use",
        "stop_sequence": null,
        "usage": {"input_tokens": 30, "output_tokens": 12}
    }))
    .await;
    fabro_test::fabro_json_snapshot!(response);
}

#[tokio::test]
async fn decode_thinking_and_redacted_thinking() {
    let response = decode_response(serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": MODEL,
        "content": [
            {"type": "thinking", "thinking": "Step one.", "signature": "sig_decode_abc"},
            {"type": "redacted_thinking", "data": "opaque-blob"},
            {"type": "text", "text": "Done."}
        ],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": 25, "output_tokens": 40}
    }))
    .await;
    fabro_test::fabro_json_snapshot!(response);
}

#[tokio::test]
async fn decode_max_tokens_stop_reason() {
    let response = decode_response(serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": MODEL,
        "content": [{"type": "text", "text": "Truncated answe"}],
        "stop_reason": "max_tokens",
        "stop_sequence": null,
        "usage": {"input_tokens": 10, "output_tokens": 128}
    }))
    .await;
    fabro_test::fabro_json_snapshot!(response);
}

#[tokio::test]
async fn decode_refusal_returns_failover_eligible_content_filter_error() {
    let err = complete_result(serde_json::json!({
        "id": "msg_refusal",
        "type": "message",
        "role": "assistant",
        "model": "claude-fable-5",
        "content": [],
        "stop_reason": "refusal",
        "stop_details": {
            "type": "refusal",
            "category": "cyber",
            "explanation": "This request was declined because it could enable cyber harm."
        },
        "usage": {"input_tokens": 412, "output_tokens": 0}
    }))
    .await
    .expect_err("refusal should be returned as an LLM error");

    assert!(err.failover_eligible());
    match &err {
        Error::Provider { kind, detail } => {
            assert_eq!(*kind, ProviderErrorKind::ContentFilter);
            assert_eq!(detail.provider, "anthropic");
            assert_eq!(detail.error_code.as_deref(), Some("refusal"));
            assert!(detail.message.contains("claude-fable-5"));
            assert!(detail.message.contains("declined"));
            assert_eq!(
                detail.raw.as_ref().unwrap()["stop_details"]["category"],
                "cyber"
            );
        }
        other => panic!("expected provider content-filter error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Stream
// ---------------------------------------------------------------------------

/// Shared setup for the happy-path text stream; the request and event halves
/// are pinned by separate tests.
async fn stream_text_happy_path_capture() -> (WireCapture, Vec<serde_json::Value>) {
    let sse = support::sse_transcript(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_stream_test","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[],"usage":{"input_tokens":11,"cache_read_input_tokens":2,"cache_creation_input_tokens":1,"output_tokens":0}}}"#,
        ),
        ("ping", r#"{"type":"ping"}"#),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    stream_capture(adapter(), &base_request(MODEL), &sse).await
}

/// The captured request pins the stream flag on the wire.
#[tokio::test]
async fn stream_text_happy_path_request() {
    let (capture, _) = stream_text_happy_path_capture().await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn stream_text_happy_path_events() {
    let (_, events) = stream_text_happy_path_capture().await;
    fabro_test::fabro_json_snapshot!(events);
}

#[tokio::test]
async fn stream_tool_call_deltas() {
    let sse = support::sse_transcript(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_stream_tool","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[],"usage":{"input_tokens":20,"output_tokens":0}}}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_01","name":"search","input":{}}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"qu"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"ery\":\"foo\"}"}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":9}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    let (_capture, events) = stream_capture(
        adapter(),
        &corpus_tools(MODEL, Some(ToolChoice::Auto)),
        &sse,
    )
    .await;
    fabro_test::fabro_json_snapshot!(events);
}

#[tokio::test]
async fn stream_thinking_with_signature_delta() {
    let sse = support::sse_transcript(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_stream_think","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[],"usage":{"input_tokens":15,"output_tokens":0}}}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me think"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig_stream_xyz"}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"4."}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":1}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":12}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    let (_capture, events) = stream_capture(adapter(), &base_request(MODEL), &sse).await;
    fabro_test::fabro_json_snapshot!(events);
}

#[tokio::test]
async fn stream_error_event_mid_stream() {
    let sse = support::sse_transcript(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_stream_err","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[],"usage":{"input_tokens":9,"output_tokens":0}}}"#,
        ),
        (
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        ),
    ]);
    let (_capture, events) = stream_capture(adapter(), &base_request(MODEL), &sse).await;
    fabro_test::fabro_json_snapshot!(events);
}

#[tokio::test]
async fn stream_refusal_returns_error_without_final_response() {
    let sse = support::sse_transcript(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_stream_refusal","type":"message","role":"assistant","model":"claude-fable-5","content":[],"usage":{"input_tokens":412,"output_tokens":0}}}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"refusal","stop_sequence":null,"stop_details":{"type":"refusal","category":"cyber","explanation":"This request was declined."}},"usage":{"output_tokens":0}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    let server = MockServer::start();
    let (mock, _slot) = mount_capture_sse(&server, "/messages", &sse);
    let adapter = adapter().with_base_url(server.base_url());
    let mut stream = adapter
        .stream(&base_request("claude-fable-5"))
        .await
        .expect("stream should start");

    let mut saw_finish = false;
    let mut refusal = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(StreamEvent::Finish { .. }) => saw_finish = true,
            Ok(_) => {}
            Err(err) => {
                refusal = Some(err);
                break;
            }
        }
    }
    mock.assert();

    assert!(!saw_finish, "refusal stream must not emit a final response");
    let err = refusal.expect("stream should yield a refusal error");
    assert!(err.failover_eligible());
    match &err {
        Error::Provider { kind, detail } => {
            assert_eq!(*kind, ProviderErrorKind::ContentFilter);
            assert_eq!(detail.error_code.as_deref(), Some("refusal"));
            assert!(detail.message.contains("claude-fable-5"));
            assert_eq!(
                detail.raw.as_ref().unwrap()["stop_details"]["category"],
                "cyber"
            );
        }
        other => panic!("expected provider content-filter error, got {other:?}"),
    }
}

/// The Anthropic decoder never synthesizes a `Finish` on byte-stream end:
/// `message_stop` is the only finisher. A transcript that ends without it
/// must produce no `Finish` event.
#[tokio::test]
async fn stream_without_message_stop_emits_no_finish() {
    let sse = support::sse_transcript(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_stream_cut","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[],"usage":{"input_tokens":11,"output_tokens":0}}}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#,
        ),
    ]);
    let (_capture, events) = stream_capture(adapter(), &base_request(MODEL), &sse).await;
    fabro_test::fabro_json_snapshot!(events);
}

// ---------------------------------------------------------------------------
// Custom-named route (the Kimi-over-anthropic shape)
// ---------------------------------------------------------------------------

/// Shared setup for a custom-named anthropic-dialect (Kimi) stream route; the
/// request and event halves are pinned by separate tests.
async fn custom_named_stream_capture() -> (WireCapture, Vec<serde_json::Value>) {
    let sse = support::sse_transcript(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_kimi","type":"message","role":"assistant","model":"kimi-test","content":[],"usage":{"input_tokens":5,"output_tokens":0}}}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":2}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    stream_capture(
        adapter().with_name("kimi"),
        &base_request("kimi-test"),
        &sse,
    )
    .await
}

/// A custom-named (Kimi) route authenticates with a bearer token and sends no
/// `anthropic-version` header. This pins that route shape on the wire.
#[tokio::test]
async fn custom_named_stream_route() {
    let (capture, _) = custom_named_stream_capture().await;
    fabro_test::fabro_json_snapshot!(capture);
}

/// Since provider-identity normalization, the streamed `Response.provider`
/// carries the configured name.
#[tokio::test]
async fn custom_named_stream_identity() {
    let (_, events) = custom_named_stream_capture().await;
    fabro_test::fabro_json_snapshot!(events);
}

/// Error events on a custom-named route carry the configured name in the
/// error detail (normalize-both decision).
#[tokio::test]
async fn custom_named_stream_error_identity() {
    let sse = support::sse_transcript(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_kimi_err","type":"message","role":"assistant","model":"kimi-test","content":[],"usage":{"input_tokens":5,"output_tokens":0}}}"#,
        ),
        (
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        ),
    ]);
    let (_capture, events) = stream_capture(
        adapter().with_name("kimi"),
        &base_request("kimi-test"),
        &sse,
    )
    .await;
    fabro_test::fabro_json_snapshot!(events);
}
