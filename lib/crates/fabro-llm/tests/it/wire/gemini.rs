//! Wire snapshots for the Gemini `generateContent` dialect. The model is
//! part of the URL path, auth is the `x-goog-api-key` header, and the
//! decoder mints synthetic UUID tool-call ids (normalized to `[UUID]` in
//! these snapshots).

use fabro_llm::provider::ProviderAdapter;
use fabro_llm::providers::GeminiAdapter;
use fabro_llm::types::{
    Message, Request, ResponseFormat, ResponseFormatType, ToolChoice, ToolDefinition,
};
use httpmock::prelude::*;

use crate::support::{
    self, WireCapture, base_request, corpus_audio_attachment, corpus_bad_file_path_attachments,
    corpus_inline_attachments, corpus_multi_turn, corpus_provider_options, corpus_response_format,
    corpus_sampling_params, corpus_thinking_round_trip, corpus_tool_round_trip, corpus_tools,
    corpus_url_attachments, json_schema_format, mount_capture, mount_capture_sse, take_capture,
};

const MODEL: &str = "gemini-test";
const COMPLETE_PATH: &str = "/models/gemini-test:generateContent";
const STREAM_PATH: &str = "/models/gemini-test:streamGenerateContent";

/// Minimal valid generateContent body for encode-side tests.
fn minimal_body() -> serde_json::Value {
    serde_json::json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "ok"}]},
            "finishReason": "STOP"
        }],
        "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
    })
}

fn adapter() -> GeminiAdapter {
    GeminiAdapter::new("test-key")
}

/// Runs `complete()` against a capture mock and returns the captured wire
/// request.
async fn encode_capture(adapter: GeminiAdapter, request: &Request) -> WireCapture {
    let server = MockServer::start();
    let (mock, slot) = mount_capture(&server, COMPLETE_PATH, minimal_body());
    let adapter = adapter.with_base_url(server.base_url());
    adapter
        .complete(request)
        .await
        .expect("complete should succeed");
    mock.assert();
    take_capture(&slot)
}

/// Runs `stream()` against an SSE transcript and returns the captured wire
/// request plus every emitted stream item as JSON (UUIDs normalized).
async fn stream_capture(
    adapter: GeminiAdapter,
    request: &Request,
    sse_body: &str,
) -> (WireCapture, Vec<serde_json::Value>) {
    let server = MockServer::start();
    let (mock, slot) = mount_capture_sse(&server, STREAM_PATH, sse_body);
    let adapter = adapter.with_base_url(server.base_url());
    let mut events = support::collect_stream_events(&adapter, request).await;
    mock.assert();
    events.iter_mut().for_each(support::normalize_uuids);
    (take_capture(&slot), events)
}

// ---------------------------------------------------------------------------
// Round trip (encode + decode)
// ---------------------------------------------------------------------------

/// Shared setup for the system+tools round trip. The decoded response is
/// returned as a UUID-normalized JSON value (gemini mints a synthetic UUID
/// for the response id); the encode and decode halves are pinned separately.
async fn system_and_tools_roundtrip() -> (WireCapture, serde_json::Value) {
    let server = MockServer::start();
    let (mock, slot) = mount_capture(
        &server,
        COMPLETE_PATH,
        serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "Hello back"}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 42,
                "candidatesTokenCount": 7,
                "cachedContentTokenCount": 10
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
    let mut response_value = serde_json::to_value(&response).expect("response should serialize");
    support::normalize_uuids(&mut response_value);
    (take_capture(&slot), response_value)
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

/// Gemini sends inline audio (the only dialect that does).
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

/// The "gemini"-namespaced provider_options merge — and the default
/// safety_settings injection it can override.
#[tokio::test]
async fn encode_provider_options_gemini_namespace() {
    let capture = encode_capture(
        adapter(),
        &corpus_provider_options(
            MODEL,
            serde_json::json!({"gemini": {"cached_content": "cachedContents/abc"}}),
        ),
    )
    .await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_provider_options_can_override_safety_settings() {
    let capture = encode_capture(
        adapter(),
        &corpus_provider_options(
            MODEL,
            serde_json::json!({"gemini": {"safety_settings": []}}),
        ),
    )
    .await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn encode_reasoning_effort_with_levels_catalog() {
    let catalog = support::catalog_from_toml(
        r#"
[providers.gemini]
display_name = "Gemini"
adapter = "gemini"
agent_profile = "gemini"

[models."gemini-test"]
provider = "gemini"
display_name = "Test Gemini"
family = "gemini"
default = true

[models."gemini-test".limits]
context_window = 200000
max_output = 4096

[models."gemini-test".features]
tools = true
vision = true
reasoning = true
reasoning_effort = "levels"
"#,
    );
    let request = Request {
        reasoning_effort: Some(fabro_llm::types::ReasoningEffort::High),
        ..base_request(MODEL)
    };
    let capture = encode_capture(adapter().with_catalog(catalog), &request).await;
    fabro_test::fabro_json_snapshot!(capture.body);
}

#[tokio::test]
async fn count_tokens_wire_shape() {
    let server = MockServer::start();
    let (mock, slot) = mount_capture(
        &server,
        "/models/gemini-test:countTokens",
        serde_json::json!({"totalTokens": 123}),
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
        .expect("gemini should count tokens");

    mock.assert();
    assert_eq!(count.input_tokens, 123);
    fabro_test::fabro_json_snapshot!(take_capture(&slot));
}

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

/// Runs `complete()` against a canned body and returns the decoded response
/// as JSON with synthetic UUIDs normalized.
async fn decode_response(body: serde_json::Value) -> serde_json::Value {
    let server = MockServer::start();
    let (mock, _slot) = mount_capture(&server, COMPLETE_PATH, body);
    let adapter = adapter().with_base_url(server.base_url());
    let response = adapter
        .complete(&base_request(MODEL))
        .await
        .expect("complete should succeed");
    mock.assert();
    let mut value = serde_json::to_value(&response).expect("response should serialize");
    support::normalize_uuids(&mut value);
    value
}

/// functionCall parts get synthetic UUID ids, preserve `thoughtSignature`,
/// and force the finish reason to ToolCalls regardless of `finishReason`.
#[tokio::test]
async fn decode_function_call_with_thought_signature() {
    let response = decode_response(serde_json::json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [
                    {"text": "Let me search."},
                    {
                        "functionCall": {"name": "search", "args": {"query": "foo"}},
                        "thoughtSignature": "sig_gemini_xyz"
                    }
                ]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {"promptTokenCount": 30, "candidatesTokenCount": 12}
    }))
    .await;
    fabro_test::fabro_json_snapshot!(response);
}

/// The Gemini usage arithmetic: input = (prompt - cached) + tool_use_prompt;
/// thoughts become reasoning tokens.
#[tokio::test]
async fn decode_usage_arithmetic() {
    let response = decode_response(serde_json::json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "ok"}]},
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 100,
            "candidatesTokenCount": 50,
            "thoughtsTokenCount": 8,
            "cachedContentTokenCount": 30,
            "toolUsePromptTokenCount": 5
        }
    }))
    .await;
    fabro_test::fabro_json_snapshot!(response);
}

/// `thought: true` text parts decode as Thinking content.
#[tokio::test]
async fn decode_thought_parts() {
    let response = decode_response(serde_json::json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [
                    {"text": "Adding the numbers.", "thought": true},
                    {"text": "4."}
                ]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {"promptTokenCount": 25, "candidatesTokenCount": 40}
    }))
    .await;
    fabro_test::fabro_json_snapshot!(response);
}

#[tokio::test]
async fn decode_max_tokens_finish_reason() {
    let length = decode_response(serde_json::json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "Trunc"}]},
            "finishReason": "MAX_TOKENS"
        }],
        "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 128}
    }))
    .await;
    fabro_test::fabro_json_snapshot!(length["finish_reason"]);
}

#[tokio::test]
async fn decode_safety_finish_reason() {
    let safety = decode_response(serde_json::json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": ""}]},
            "finishReason": "SAFETY"
        }],
        "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 0}
    }))
    .await;
    fabro_test::fabro_json_snapshot!(safety["finish_reason"]);
}

// ---------------------------------------------------------------------------
// Stream
// ---------------------------------------------------------------------------

/// Shared setup for the happy-path text stream; the request and event halves
/// are pinned by separate tests.
async fn stream_text_happy_path_capture() -> (WireCapture, Vec<serde_json::Value>) {
    let sse = support::sse_data_transcript(&[
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hel"}]}}]}"#,
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"lo"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":11,"candidatesTokenCount":5}}"#,
    ]);
    stream_capture(adapter(), &base_request(MODEL), &sse).await
}

/// The captured request pins model-in-URL and `?alt=sse` on the wire.
#[tokio::test]
async fn stream_text_happy_path_request() {
    let (capture, _) = stream_text_happy_path_capture().await;
    fabro_test::fabro_json_snapshot!(capture);
}

#[tokio::test]
async fn stream_text_happy_path_events() {
    let (_, events) = stream_text_happy_path_capture().await;
    fabro_test::fabro_json_snapshot!(events);
}

#[tokio::test]
async fn stream_function_call() {
    let sse = support::sse_data_transcript(&[
        r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"search","args":{"query":"foo"}},"thoughtSignature":"sig_stream_g"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":20,"candidatesTokenCount":9}}"#,
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
async fn stream_thought_parts() {
    let sse = support::sse_data_transcript(&[
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Let me think","thought":true}]}}]}"#,
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"4."}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":15,"candidatesTokenCount":12,"thoughtsTokenCount":6}}"#,
    ]);
    let (_capture, events) = stream_capture(adapter(), &base_request(MODEL), &sse).await;
    fabro_test::fabro_json_snapshot!(events);
}

/// The Gemini decoder synthesizes a `Finish` on byte-stream end
/// unconditionally — even when no chunk carried a `finishReason`.
#[tokio::test]
async fn stream_end_synthesizes_finish_without_finish_reason() {
    let sse = support::sse_data_transcript(&[
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]}}]}"#,
    ]);
    let (_capture, events) = stream_capture(adapter(), &base_request(MODEL), &sse).await;
    fabro_test::fabro_json_snapshot!(events);
}
