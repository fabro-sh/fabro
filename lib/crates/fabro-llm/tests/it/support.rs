//! Shared helpers for capturing the wire requests adapters send, plus the
//! canonical request corpus pinned across all four provider dialects.

use std::sync::{Arc, Mutex};

use fabro_llm::provider::ProviderAdapter;
use fabro_llm::types::{
    AudioData, ContentPart, DocumentData, ImageData, Message, Request, ResponseFormat, Role,
    ThinkingData, ToolCall, ToolChoice, ToolDefinition, ToolResult,
};
use fabro_model::Catalog;
use fabro_model::catalog::LlmCatalogSettings;
use httpmock::prelude::*;

// ---------------------------------------------------------------------------
// Wire capture
// ---------------------------------------------------------------------------

/// One captured wire request, normalized for snapshot stability.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct WireCapture {
    pub(crate) method:  String,
    pub(crate) path:    String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body:    serde_json::Value,
}

/// Shared slot the matcher closure writes the captured request into.
pub(crate) type CaptureSlot = Arc<Mutex<Option<WireCapture>>>;

fn capture_request(req: &HttpMockRequest) -> WireCapture {
    let mut headers: Vec<(String, String)> = req
        .headers_vec()
        .iter()
        .map(|(name, value)| {
            let name = name.to_ascii_lowercase();
            let value = match name.as_str() {
                // The mock server binds a random port.
                "host" => "[host]".to_string(),
                // Carries a client version that would churn snapshots.
                "user-agent" => "[user-agent]".to_string(),
                _ => value.clone(),
            };
            (name, value)
        })
        .collect();
    headers.sort();

    let path = match req.uri().query() {
        Some(query) => format!("{}?{}", req.uri().path(), query),
        None => req.uri().path().to_string(),
    };

    WireCapture {
        method: req.method_str().to_string(),
        path,
        headers,
        body: serde_json::from_str(&req.body_string()).expect("request body should be JSON"),
    }
}

/// Mounts a mock on `path` that captures the full request into the returned
/// slot and responds with the JSON `response_body`.
pub(crate) fn mount_capture<'a>(
    server: &'a MockServer,
    path: &'static str,
    response_body: serde_json::Value,
) -> (httpmock::Mock<'a>, CaptureSlot) {
    let slot: CaptureSlot = Arc::new(Mutex::new(None));
    let writer = Arc::clone(&slot);
    let mock = server.mock(move |when, then| {
        when.method(POST)
            .path(path)
            .is_true(move |req: &HttpMockRequest| {
                *writer.lock().unwrap() = Some(capture_request(req));
                true
            });
        then.status(200)
            .header("content-type", "application/json")
            .json_body(response_body);
    });
    (mock, slot)
}

/// Like [`mount_capture`] but responds with a raw SSE transcript.
pub(crate) fn mount_capture_sse<'a>(
    server: &'a MockServer,
    path: &'static str,
    sse_body: &str,
) -> (httpmock::Mock<'a>, CaptureSlot) {
    let slot: CaptureSlot = Arc::new(Mutex::new(None));
    let writer = Arc::clone(&slot);
    let body = sse_body.to_string();
    let mock = server.mock(move |when, then| {
        when.method(POST)
            .path(path)
            .is_true(move |req: &HttpMockRequest| {
                *writer.lock().unwrap() = Some(capture_request(req));
                true
            });
        then.status(200)
            .header("content-type", "text/event-stream")
            .body(body.clone());
    });
    (mock, slot)
}

pub(crate) fn take_capture(slot: &CaptureSlot) -> WireCapture {
    slot.lock()
        .unwrap()
        .take()
        .expect("matcher should have captured the request")
}

/// Drives `adapter.stream(request)` to completion and returns every emitted
/// item as JSON: `Ok` events serialize verbatim (the public SSE wire shape);
/// `Err` items pin the message plus the failover/retry flags consumers key on.
pub(crate) async fn collect_stream_events(
    adapter: &dyn ProviderAdapter,
    request: &Request,
) -> Vec<serde_json::Value> {
    use futures::StreamExt;

    let mut stream = adapter.stream(request).await.expect("stream should start");
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(match item {
            Ok(event) => serde_json::to_value(&event).expect("event should serialize"),
            Err(error) => serde_json::json!({
                "stream_item_error": error.to_string(),
                "retryable": error.retryable(),
                "failover_eligible": error.failover_eligible(),
            }),
        });
    }
    events
}

/// Builds a catalog from inline TOML (same `LlmCatalogSettings` schema as the
/// shipped catalog files).
pub(crate) fn catalog_from_toml(source: &str) -> Arc<Catalog> {
    let settings: LlmCatalogSettings = toml::from_str(source).expect("catalog TOML should parse");
    Arc::new(Catalog::from_settings(&settings).expect("catalog should build"))
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Replaces UUID-shaped strings with `[UUID]` for snapshot stability — the
/// Gemini decoder mints synthetic `Uuid::new_v4()` tool-call ids.
pub(crate) fn normalize_uuids(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) if is_uuid(s) => "[UUID]".clone_into(s),
        serde_json::Value::Array(items) => items.iter_mut().for_each(normalize_uuids),
        serde_json::Value::Object(map) => map.values_mut().for_each(normalize_uuids),
        _ => {}
    }
}

/// Renders `(event, data)` pairs as an SSE transcript with `event:` lines
/// (the Anthropic framing).
pub(crate) fn sse_transcript(events: &[(&str, &str)]) -> String {
    use std::fmt::Write;

    events.iter().fold(String::new(), |mut out, (event, data)| {
        let _ = writeln!(out, "event: {event}\ndata: {data}\n");
        out
    })
}

/// Renders data-only SSE lines (the OpenAI/Gemini framing).
pub(crate) fn sse_data_transcript(lines: &[&str]) -> String {
    use std::fmt::Write;

    lines.iter().fold(String::new(), |mut out, data| {
        let _ = writeln!(out, "data: {data}\n");
        out
    })
}

// ---------------------------------------------------------------------------
// Canonical request corpus
//
// Each constructor returns one canonical `Request` shape that every dialect
// file pins through its own adapter. Keep these stable: editing a corpus
// request invalidates the pinned wire snapshots in all four dialect files.
// ---------------------------------------------------------------------------

pub(crate) fn base_request(model: &str) -> Request {
    Request {
        model:            model.to_string(),
        messages:         vec![Message::user("Hello")],
        provider:         None,
        tools:            None,
        tool_choice:      None,
        response_format:  None,
        temperature:      None,
        top_p:            None,
        max_tokens:       Some(128),
        stop_sequences:   None,
        reasoning_effort: None,
        speed:            None,
        metadata:         None,
        provider_options: None,
    }
}

/// Multi-turn conversation: system + user/assistant/user.
pub(crate) fn corpus_multi_turn(model: &str) -> Request {
    Request {
        messages: vec![
            Message::system("You are a terse assistant."),
            Message::user("What is the capital of France?"),
            Message::assistant("Paris."),
            Message::user("And of Spain?"),
        ],
        ..base_request(model)
    }
}

/// Two tools plus an optional tool choice.
pub(crate) fn corpus_tools(model: &str, tool_choice: Option<ToolChoice>) -> Request {
    Request {
        tools: Some(vec![
            ToolDefinition::function(
                "search",
                "Search files",
                serde_json::json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"]
                }),
            ),
            ToolDefinition::function(
                "read_file",
                "Read a file by path",
                serde_json::json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}}
                }),
            ),
        ]),
        tool_choice,
        ..base_request(model)
    }
}

/// A full tool round trip: assistant emits two tool calls, the tool turn
/// returns one success carrying an image and one error result.
pub(crate) fn corpus_tool_round_trip(model: &str) -> Request {
    let mut image_result = ToolResult::success("call_1", serde_json::json!({"matches": 2}));
    image_result.image_data = Some(b"fake-screenshot-bytes".to_vec());
    image_result.image_media_type = Some("image/png".to_string());

    let mut request = corpus_tools(model, None);
    request.messages = vec![
        Message::user("Find foo and read /tmp/x"),
        Message {
            role:         Role::Assistant,
            content:      vec![
                ContentPart::text("Let me check."),
                ContentPart::ToolCall(ToolCall::new(
                    "call_1",
                    "search",
                    serde_json::json!({"query": "foo"}),
                )),
                ContentPart::ToolCall(ToolCall::new(
                    "call_2",
                    "read_file",
                    serde_json::json!({"path": "/tmp/x"}),
                )),
            ],
            name:         None,
            tool_call_id: None,
        },
        Message {
            role:         Role::Tool,
            content:      vec![ContentPart::ToolResult(image_result)],
            name:         None,
            tool_call_id: Some("call_1".to_string()),
        },
        Message::tool_result(
            "call_2",
            serde_json::Value::String("file not found".to_string()),
            true,
        ),
    ];
    request
}

/// Assistant thinking block with a signature, round-tripped back as history.
pub(crate) fn corpus_thinking_round_trip(model: &str) -> Request {
    Request {
        messages: vec![
            Message::user("Think step by step: what is 2+2?"),
            Message {
                role:         Role::Assistant,
                content:      vec![
                    ContentPart::Thinking(ThinkingData {
                        text:      "The user wants 2+2, which is 4.".to_string(),
                        signature: Some("sig_test_abc123".to_string()),
                        redacted:  false,
                    }),
                    ContentPart::text("4."),
                ],
                name:         None,
                tool_call_id: None,
            },
            Message::user("Now 3+3?"),
        ],
        ..base_request(model)
    }
}

/// Image and document attachments as inline bytes (no file I/O involved).
pub(crate) fn corpus_inline_attachments(model: &str) -> Request {
    Request {
        messages: vec![Message {
            role:         Role::User,
            content:      vec![
                ContentPart::text("Describe these attachments."),
                ContentPart::Image(ImageData {
                    url:        None,
                    data:       Some(b"fake-png-bytes".to_vec()),
                    media_type: Some("image/png".to_string()),
                    detail:     None,
                }),
                ContentPart::Document(DocumentData {
                    url:        None,
                    data:       Some(b"fake-pdf-bytes".to_vec()),
                    media_type: Some("application/pdf".to_string()),
                    file_name:  Some("report.pdf".to_string()),
                }),
            ],
            name:         None,
            tool_call_id: None,
        }],
        ..base_request(model)
    }
}

/// Image and document attachments as non-file https URLs. Each dialect has
/// its own URL-passthrough wire shape; resolving these to inline data would
/// be a wire change.
pub(crate) fn corpus_url_attachments(model: &str) -> Request {
    Request {
        messages: vec![Message {
            role:         Role::User,
            content:      vec![
                ContentPart::text("Describe these attachments."),
                ContentPart::Image(ImageData {
                    url:        Some("https://example.com/picture.png".to_string()),
                    data:       None,
                    media_type: Some("image/png".to_string()),
                    detail:     None,
                }),
                ContentPart::Document(DocumentData {
                    url:        Some("https://example.com/report.pdf".to_string()),
                    data:       None,
                    media_type: Some("application/pdf".to_string()),
                    file_name:  Some("report.pdf".to_string()),
                }),
            ],
            name:         None,
            tool_call_id: None,
        }],
        ..base_request(model)
    }
}

/// Attachments referencing file paths that do not exist. Today every adapter
/// silently drops the part on load failure (`Err(_) => None`) and sends the
/// rest of the request; these requests pin that contract.
pub(crate) fn corpus_bad_file_path_attachments(model: &str) -> Request {
    Request {
        messages: vec![Message {
            role:         Role::User,
            content:      vec![
                ContentPart::text("Describe these attachments."),
                ContentPart::Image(ImageData {
                    url:        Some("/nonexistent/fabro-wire-pin.png".to_string()),
                    data:       None,
                    media_type: Some("image/png".to_string()),
                    detail:     None,
                }),
                ContentPart::Document(DocumentData {
                    url:        Some("/nonexistent/fabro-wire-pin.pdf".to_string()),
                    data:       None,
                    media_type: Some("application/pdf".to_string()),
                    file_name:  Some("missing.pdf".to_string()),
                }),
            ],
            name:         None,
            tool_call_id: None,
        }],
        ..base_request(model)
    }
}

/// Inline audio attachment (support differs per dialect: gemini sends it,
/// openai-responses falls back to text, anthropic/compat drop or warn).
pub(crate) fn corpus_audio_attachment(model: &str) -> Request {
    Request {
        messages: vec![Message {
            role:         Role::User,
            content:      vec![
                ContentPart::text("Transcribe this."),
                ContentPart::Audio(AudioData {
                    url:        None,
                    data:       Some(b"fake-wav-bytes".to_vec()),
                    media_type: Some("audio/wav".to_string()),
                }),
            ],
            name:         None,
            tool_call_id: None,
        }],
        ..base_request(model)
    }
}

/// Response-format request (callers pass each of the three kinds).
pub(crate) fn corpus_response_format(model: &str, format: ResponseFormat) -> Request {
    Request {
        response_format: Some(format),
        ..base_request(model)
    }
}

/// A JSON-schema response format with `strict` set. The schema is passed raw
/// (no name/schema wrapper) — the shape `generate_object` produces.
pub(crate) fn json_schema_format() -> ResponseFormat {
    ResponseFormat {
        kind:        fabro_llm::types::ResponseFormatType::JsonSchema,
        json_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}},
            "required": ["answer"]
        })),
        strict:      true,
    }
}

/// Sampling parameters: temperature, top_p, stop sequences, and metadata.
/// Metadata deliberately holds a single key — `HashMap` iteration order would
/// make multi-key snapshots nondeterministic.
pub(crate) fn corpus_sampling_params(model: &str) -> Request {
    Request {
        temperature: Some(0.7),
        top_p: Some(0.9),
        stop_sequences: Some(vec!["END".to_string()]),
        metadata: Some(std::collections::HashMap::from([(
            "trace_id".to_string(),
            "trace-123".to_string(),
        )])),
        ..base_request(model)
    }
}

/// Provider-options escape hatch (callers pass the dialect's namespace key).
pub(crate) fn corpus_provider_options(model: &str, options: serde_json::Value) -> Request {
    Request {
        provider_options: Some(options),
        ..base_request(model)
    }
}
