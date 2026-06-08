//! Wire snapshots for the OpenAI Responses API dialect (`POST /responses`).

use fabro_llm::provider::ProviderAdapter;
use fabro_llm::providers::OpenAiAdapter;
use fabro_llm::types::{
    ContentPart, Message, Request, ResponseFormat, ResponseFormatType, Role, ToolCall, ToolChoice,
    ToolDefinition,
};
use httpmock::prelude::*;

use crate::support::{
    self, WireCapture, base_request, corpus_audio_attachment, corpus_bad_file_path_attachments,
    corpus_inline_attachments, corpus_multi_turn, corpus_provider_options, corpus_response_format,
    corpus_sampling_params, corpus_thinking_round_trip, corpus_tool_round_trip, corpus_tools,
    corpus_url_attachments, json_schema_format, mount_capture, mount_capture_sse, take_capture,
};

const MODEL: &str = "gpt-test";

/// Minimal valid Responses API body for encode-side tests.
fn minimal_body() -> serde_json::Value {
    serde_json::json!({
        "id": "resp_test",
        "object": "response",
        "model": MODEL,
        "status": "completed",
        "output": [{
            "type": "message",
            "role": "assistant",
            "id": "msg_out",
            "content": [{"type": "output_text", "text": "ok"}]
        }],
        "usage": {"input_tokens": 1, "output_tokens": 1}
    })
}

fn adapter() -> OpenAiAdapter {
    OpenAiAdapter::new("test-key")
}

/// Runs `complete()` against a capture mock and returns the captured wire
/// request.
async fn encode_capture(adapter: OpenAiAdapter, request: &Request) -> WireCapture {
    let server = MockServer::start();
    let (mock, slot) = mount_capture(&server, "/responses", minimal_body());
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
    adapter: OpenAiAdapter,
    request: &Request,
    sse_body: &str,
) -> (WireCapture, Vec<serde_json::Value>) {
    let server = MockServer::start();
    let (mock, slot) = mount_capture_sse(&server, "/responses", sse_body);
    let adapter = adapter.with_base_url(server.base_url());
    let events = support::collect_stream_events(&adapter, request).await;
    mock.assert();
    (take_capture(&slot), events)
}

// ---------------------------------------------------------------------------
// Round trip (encode + decode)
// ---------------------------------------------------------------------------

/// Shared setup for the system+tools round trip; the encode and decode halves
/// are pinned by separate tests.
async fn system_and_tools_roundtrip() -> (WireCapture, fabro_llm::types::Response) {
    let server = MockServer::start();
    let (mock, slot) = mount_capture(
        &server,
        "/responses",
        serde_json::json!({
            "id": "resp_test",
            "object": "response",
            "model": MODEL,
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "id": "msg_out",
                "content": [{"type": "output_text", "text": "Hello back"}]
            }],
            "usage": {
                "input_tokens": 42,
                "output_tokens": 7,
                "input_tokens_details": {"cached_tokens": 10},
                "output_tokens_details": {"reasoning_tokens": 3}
            }
        }),
    );

    let adapter = adapter().with_base_url(server.base_url());
    let request = Request {
        messages: vec![Message::system("Be concise"), Message::user("Hello")],
        tools: Some(vec![ToolDefinition::function(
            "search",
            "Search files",
            serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}}),
        )]),
        temperature: Some(0.5),
        ..base_request(MODEL)
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
    fabro_test::fabro_json_snapshot!(capture.body);
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

/// A tool call that decoded with an item-level id (`fc_…`) in
/// provider_metadata re-encodes with the dual ids split correctly.
#[tokio::test]
async fn encode_dual_id_tool_round_trip() {
    let mut tool_call = ToolCall::new("call_abc", "search", serde_json::json!({"query": "foo"}));
    tool_call.provider_metadata = Some(serde_json::json!({"id": "fc_123"}));
    let mut request = corpus_tools(MODEL, None);
    request.messages = vec![
        Message::user("Find foo"),
        Message {
            role:         Role::Assistant,
            content:      vec![ContentPart::ToolCall(tool_call)],
            name:         None,
            tool_call_id: None,
        },
        Message::tool_result(
            "call_abc",
            serde_json::Value::String("2 matches".to_string()),
            false,
        ),
    ];
    let capture = encode_capture(adapter(), &request).await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

/// Opaque OpenAI items (reasoning / message) round-trip verbatim into the
/// input array.
#[tokio::test]
async fn encode_opaque_items_round_trip() {
    let request = Request {
        messages: vec![
            Message::user("Think about 2+2."),
            Message {
                role:         Role::Assistant,
                content:      vec![
                    ContentPart::Other {
                        kind: ContentPart::OPENAI_REASONING.to_string(),
                        data: serde_json::json!({
                            "type": "reasoning",
                            "id": "rs_1",
                            "summary": [{"type": "summary_text", "text": "Adding."}]
                        }),
                    },
                    ContentPart::Other {
                        kind: ContentPart::OPENAI_MESSAGE.to_string(),
                        data: serde_json::json!({
                            "type": "message",
                            "role": "assistant",
                            "id": "msg_1",
                            "content": [{"type": "output_text", "text": "4."}]
                        }),
                    },
                ],
                name:         None,
                tool_call_id: None,
            },
            Message::user("Now 3+3?"),
        ],
        ..base_request(MODEL)
    };
    let capture = encode_capture(adapter(), &request).await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

/// Canonical Thinking parts (anthropic-style) — distinct from the opaque
/// reasoning round-trip above.
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
async fn encode_provider_options_openai_namespace() {
    let capture = encode_capture(
        adapter(),
        &corpus_provider_options(MODEL, serde_json::json!({"openai": {"seed": 42}})),
    )
    .await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_reasoning_effort_with_levels_catalog() {
    let catalog = support::catalog_from_toml(
        r#"
[providers.openai]
display_name = "OpenAI"
adapter = "openai"
agent_profile = "openai"

[models."test-gpt"]
provider = "openai"
display_name = "Test GPT"
family = "gpt"
default = true

[models."test-gpt".limits]
context_window = 200000
max_output = 4096

[models."test-gpt".features]
tools = true
vision = true
reasoning = true
reasoning_effort = "levels"
"#,
    );
    let request = Request {
        reasoning_effort: Some(fabro_llm::types::ReasoningEffort::High),
        ..base_request("test-gpt")
    };
    let capture = encode_capture(adapter().with_catalog(catalog), &request).await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

/// Codex mode forces streaming for `complete()` and omits sampling params
/// from the encoded body.
#[tokio::test]
async fn encode_codex_mode_forces_streaming_and_omits_params() {
    let sse = support::sse_data_transcript(&[
        r#"{"type":"response.created","response":{"id":"resp_codex","model":"gpt-test"}}"#,
        r#"{"type":"response.output_text.delta","delta":"ok"}"#,
        r#"{"type":"response.completed","response":{"id":"resp_codex","model":"gpt-test","status":"completed","output":[],"usage":{"input_tokens":5,"output_tokens":2}}}"#,
    ]);
    let server = MockServer::start();
    let (mock, slot) = mount_capture_sse(&server, "/responses", &sse);
    let adapter = OpenAiAdapter::new("test-key")
        .with_codex_mode()
        .with_base_url(server.base_url());
    let request = Request {
        messages: vec![Message::system("Be concise"), Message::user("Hello")],
        temperature: Some(0.5),
        top_p: Some(0.9),
        ..base_request(MODEL)
    };
    adapter.complete(&request).await.unwrap();
    mock.assert();
    fabro_test::fabro_json_snapshot!(take_capture(&slot));
}

#[tokio::test]
async fn count_tokens_wire_shape() {
    let server = MockServer::start();
    let (mock, slot) = mount_capture(
        &server,
        "/responses/input_tokens",
        serde_json::json!({"input_tokens": 123, "object": "response.input_tokens"}),
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
        .expect("openai should count tokens");

    mock.assert();
    assert_eq!(count.input_tokens, 123);
    fabro_test::fabro_json_snapshot!(take_capture(&slot));
}

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

async fn decode_response(body: serde_json::Value) -> fabro_llm::types::Response {
    let server = MockServer::start();
    let (mock, _slot) = mount_capture(&server, "/responses", body);
    let adapter = adapter().with_base_url(server.base_url());
    let response = adapter
        .complete(&base_request(MODEL))
        .await
        .expect("complete should succeed");
    mock.assert();
    response
}

/// The Responses usage arithmetic: cached tokens are subtracted from input,
/// reasoning tokens from output.
#[tokio::test]
async fn decode_usage_subtracts_cached_and_reasoning() {
    let response = decode_response(serde_json::json!({
        "id": "resp_test",
        "object": "response",
        "model": MODEL,
        "status": "completed",
        "output": [{
            "type": "message",
            "role": "assistant",
            "id": "msg_out",
            "content": [{"type": "output_text", "text": "ok"}]
        }],
        "usage": {
            "input_tokens": 100,
            "output_tokens": 50,
            "input_tokens_details": {"cached_tokens": 80},
            "output_tokens_details": {"reasoning_tokens": 20}
        }
    }))
    .await;
    fabro_test::fabro_json_snapshot!(response);
}

/// Reasoning and function_call output items: reasoning becomes an opaque
/// round-trip part, function_call splits dual ids into id + metadata.
#[tokio::test]
async fn decode_reasoning_and_function_call_items() {
    let response = decode_response(serde_json::json!({
        "id": "resp_test",
        "object": "response",
        "model": MODEL,
        "status": "completed",
        "output": [
            {
                "type": "reasoning",
                "id": "rs_1",
                "summary": [{"type": "summary_text", "text": "Searching."}]
            },
            {
                "type": "function_call",
                "id": "fc_123",
                "call_id": "call_abc",
                "name": "search",
                "arguments": "{\"query\":\"foo\"}"
            }
        ],
        "usage": {"input_tokens": 30, "output_tokens": 12}
    }))
    .await;
    fabro_test::fabro_json_snapshot!(response);
}

#[tokio::test]
async fn decode_incomplete_status_maps_to_length() {
    let response = decode_response(serde_json::json!({
        "id": "resp_test",
        "object": "response",
        "model": MODEL,
        "status": "incomplete",
        "output": [{
            "type": "message",
            "role": "assistant",
            "id": "msg_out",
            "content": [{"type": "output_text", "text": "Truncated"}]
        }],
        "usage": {"input_tokens": 10, "output_tokens": 128}
    }))
    .await;
    fabro_test::fabro_json_snapshot!(response);
}

// ---------------------------------------------------------------------------
// Stream
// ---------------------------------------------------------------------------

/// Shared setup for the happy-path text stream; the request and event halves
/// are pinned by separate tests.
async fn stream_text_happy_path_capture() -> (WireCapture, Vec<serde_json::Value>) {
    let sse = support::sse_data_transcript(&[
        r#"{"type":"response.created","response":{"id":"resp_stream","model":"gpt-test"}}"#,
        r#"{"type":"response.output_text.delta","delta":"Hel"}"#,
        r#"{"type":"response.output_text.delta","delta":"lo"}"#,
        r#"{"type":"response.completed","response":{"id":"resp_stream","model":"gpt-test","status":"completed","output":[],"usage":{"input_tokens":11,"output_tokens":5,"input_tokens_details":{"cached_tokens":2},"output_tokens_details":{"reasoning_tokens":1}}}}"#,
    ]);
    stream_capture(adapter(), &base_request(MODEL), &sse).await
}

/// The captured request pins the stream flag (and `include`) on the wire.
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
    let sse = support::sse_data_transcript(&[
        r#"{"type":"response.created","response":{"id":"resp_stream","model":"gpt-test"}}"#,
        r#"{"type":"response.function_call_arguments.delta","item_id":"fc_123","call_id":"call_abc","name":"search","delta":"{\"qu"}"#,
        r#"{"type":"response.function_call_arguments.delta","item_id":"fc_123","call_id":"call_abc","delta":"ery\":\"foo\"}"}"#,
        r#"{"type":"response.output_item.done","item":{"type":"function_call","id":"fc_123","call_id":"call_abc","name":"search","arguments":"{\"query\":\"foo\"}"}}"#,
        r#"{"type":"response.completed","response":{"id":"resp_stream","model":"gpt-test","status":"completed","output":[],"usage":{"input_tokens":20,"output_tokens":9}}}"#,
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
async fn stream_reasoning_summary_deltas() {
    let sse = support::sse_data_transcript(&[
        r#"{"type":"response.created","response":{"id":"resp_stream","model":"gpt-test"}}"#,
        r#"{"type":"response.reasoning_summary_text.delta","delta":"Let me "}"#,
        r#"{"type":"response.reasoning_summary_text.delta","delta":"think"}"#,
        r#"{"type":"response.output_text.delta","delta":"4."}"#,
        r#"{"type":"response.completed","response":{"id":"resp_stream","model":"gpt-test","status":"completed","output":[],"usage":{"input_tokens":15,"output_tokens":12,"output_tokens_details":{"reasoning_tokens":8}}}}"#,
    ]);
    let (_capture, events) = stream_capture(adapter(), &base_request(MODEL), &sse).await;
    fabro_test::fabro_json_snapshot!(events);
}

#[tokio::test]
async fn stream_failed_event_maps_to_error() {
    let sse = support::sse_data_transcript(&[
        r#"{"type":"response.created","response":{"id":"resp_stream","model":"gpt-test"}}"#,
        r#"{"type":"response.failed","response":{"id":"resp_stream","error":{"code":"server_error","message":"boom"}}}"#,
    ]);
    let (_capture, events) = stream_capture(adapter(), &base_request(MODEL), &sse).await;
    fabro_test::fabro_json_snapshot!(events);
}

/// `response.incomplete` finishes the stream with `Length`.
#[tokio::test]
async fn stream_incomplete_maps_to_length() {
    let sse = support::sse_data_transcript(&[
        r#"{"type":"response.created","response":{"id":"resp_stream","model":"gpt-test"}}"#,
        r#"{"type":"response.output_text.delta","delta":"Trunc"}"#,
        r#"{"type":"response.incomplete","response":{"id":"resp_stream","model":"gpt-test","status":"incomplete","output":[],"usage":{"input_tokens":10,"output_tokens":128}}}"#,
    ]);
    let (_capture, events) = stream_capture(adapter(), &base_request(MODEL), &sse).await;
    fabro_test::fabro_json_snapshot!(events);
}
