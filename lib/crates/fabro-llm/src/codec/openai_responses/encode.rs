//! Request encoding: canonical request → OpenAI Responses API body.
//!
//! Pure and sync. File-backed image attachments are resolved to inline data by
//! `attachments::resolve` in the adapter *before* encode runs, so the content
//! translation here never touches the filesystem.

use std::collections::HashSet;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use super::wire::ApiRequest;
use crate::codec::{CodecCtx, EncodedRequest};
use crate::types::{
    ContentPart, Message, ResponseFormat, ResponseFormatType, Role, ToolChoice, ToolDefinition,
};

// --- Public entry points -----------------------------------------------------

pub(super) fn encode(ctx: &CodecCtx<'_>, stream: bool) -> EncodedRequest {
    EncodedRequest {
        body:     build_body(ctx, stream),
        endpoint: "/responses".to_string(),
        headers:  Vec::new(),
    }
}

pub(super) fn encode_count_tokens(ctx: &CodecCtx<'_>) -> EncodedRequest {
    EncodedRequest {
        body:     filter_input_tokens_request_body(build_body(ctx, false)),
        endpoint: "/responses/input_tokens".to_string(),
        headers:  Vec::new(),
    }
}

/// Serialize the API request and merge any `provider_options.openai` keys into
/// the body (overrides win, matching the long-standing contract).
fn build_body(ctx: &CodecCtx<'_>, stream: bool) -> serde_json::Value {
    let api_request = build_api_request(ctx, stream);
    let mut body = serde_json::to_value(&api_request).unwrap_or_else(|_| serde_json::json!({}));

    if let Some(openai_opts) = ctx
        .request
        .provider_options
        .as_ref()
        .and_then(|opts| opts.get("openai"))
    {
        if let (Some(base), Some(overrides)) = (body.as_object_mut(), openai_opts.as_object()) {
            for (key, value) in overrides {
                base.insert(key.clone(), value.clone());
            }
        }
    }

    body
}

/// Build an `ApiRequest` from the canonical request.
///
/// When the route is in codex mode (`ctx.params.openai_codex`), unsupported
/// fields (`temperature`, `max_output_tokens`, `top_p`) are omitted and empty
/// instructions are sent as `""` (required by the Codex endpoint).
fn build_api_request(ctx: &CodecCtx<'_>, stream: bool) -> ApiRequest {
    let request = ctx.request;
    let codex_mode = ctx.params.openai_codex;

    let (instructions, input) = translate_input(&request.messages);
    let api_tools = request.tools.as_ref().map(|t| translate_tools(t));
    let tool_choice = request.tool_choice.as_ref().map(translate_tool_choice);
    let reasoning = request
        .reasoning_effort
        .as_ref()
        .map(|effort| serde_json::json!({"effort": <&'static str>::from(*effort)}));
    let text = request
        .response_format
        .as_ref()
        .and_then(translate_response_format);

    let include = vec!["reasoning.encrypted_content".to_string()];

    let instructions = if codex_mode {
        Some(instructions.unwrap_or_default())
    } else {
        instructions
    };

    ApiRequest {
        model: ctx.deployment_id.to_string(),
        input,
        instructions,
        temperature: if codex_mode {
            None
        } else {
            request.temperature
        },
        max_output_tokens: if codex_mode { None } else { request.max_tokens },
        top_p: if codex_mode { None } else { request.top_p },
        tools: api_tools,
        tool_choice,
        reasoning,
        text,
        stop: request.stop_sequences.clone(),
        metadata: request.metadata.clone(),
        // store: false means output items are not persisted server-side.
        // Request encrypted reasoning content on every turn so reasoning items
        // from models that emit them by default can round-trip statelessly.
        store: false,
        include,
        stream,
    }
}

/// Project a full request body down to the fields the
/// `/responses/input_tokens` endpoint accepts.
fn filter_input_tokens_request_body(mut body: serde_json::Value) -> serde_json::Value {
    const ALLOWED_FIELDS: &[&str] = &[
        "conversation",
        "input",
        "instructions",
        "model",
        "parallel_tool_calls",
        "previous_response_id",
        "reasoning",
        "text",
        "tool_choice",
        "tools",
        "truncation",
    ];

    let Some(obj) = body.as_object_mut() else {
        return serde_json::json!({});
    };
    obj.retain(|key, _| ALLOWED_FIELDS.contains(&key.as_str()));
    body
}

// --- Content / message / tool translation ------------------------------------

/// Translate unified messages to Responses API `input` array format. Sync:
/// file-backed image attachments are already resolved to inline data upstream.
pub(super) fn translate_input(messages: &[Message]) -> (Option<String>, Vec<serde_json::Value>) {
    let mut instructions_parts: Vec<String> = Vec::new();
    let mut input: Vec<serde_json::Value> = Vec::new();
    let mut custom_call_ids: HashSet<String> = HashSet::new();

    for msg in messages {
        match msg.role {
            Role::System | Role::Developer => {
                instructions_parts.push(msg.text());
            }
            Role::User => {
                let mut content = Vec::new();
                for part in &msg.content {
                    let maybe_content = match part {
                        ContentPart::Text(text) => {
                            Some(serde_json::json!({"type": "input_text", "text": text}))
                        }
                        ContentPart::Image(img) => match &img.url {
                            Some(url) => {
                                Some(serde_json::json!({"type": "input_image", "image_url": url}))
                            }
                            None => img.data.as_ref().map(|data| {
                                let mime = img.media_type.as_deref().unwrap_or("image/png");
                                let b64 = BASE64_STANDARD.encode(data);
                                serde_json::json!({
                                    "type": "input_image",
                                    "image_url": format!("data:{mime};base64,{b64}"),
                                })
                            }),
                        },
                        ContentPart::Audio(_) => Some(
                            serde_json::json!({"type": "input_text", "text": "[Audio content not supported by this provider]"}),
                        ),
                        ContentPart::Document(doc) => {
                            let desc = doc.file_name.as_ref().map_or_else(
                                || "[Document content not supported by this provider]".to_string(),
                                |name| format!("[Document '{name}': content type not supported by this provider]"),
                            );
                            Some(serde_json::json!({"type": "input_text", "text": desc}))
                        }
                        _ => None,
                    };
                    if let Some(content_part) = maybe_content {
                        content.push(content_part);
                    }
                }
                if !content.is_empty() {
                    input.push(serde_json::json!({
                        "type": "message",
                        "role": "user",
                        "content": content,
                    }));
                }
            }
            Role::Assistant => {
                // If we have a preserved opaque message item (with id/status), use
                // it instead of constructing a new message from Text parts.  This is
                // required so that reasoning items can find their "required following
                // item" during Responses API round-tripping.
                let has_opaque_message = msg.content.iter().any(|p| {
                    matches!(p, ContentPart::Other { kind, .. } if kind == ContentPart::OPENAI_MESSAGE)
                });
                for part in &msg.content {
                    match part {
                        ContentPart::Text(text) if !has_opaque_message => {
                            input.push(serde_json::json!({
                                "type": "message",
                                "role": "assistant",
                                "content": [{"type": "output_text", "text": text}],
                            }));
                        }
                        ContentPart::ToolCall(tc) if !tc.name.is_empty() => {
                            // Use the item-level ID (fc_xxx) for the `id` field;
                            // fall back to tc.id if no provider_metadata was stored.
                            let item_id = tc
                                .provider_metadata
                                .as_ref()
                                .and_then(|m| m.get("id"))
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or(&tc.id);
                            if tc.tool_type == "custom" {
                                custom_call_ids.insert(tc.id.clone());
                                let raw_input = tc.raw_arguments.as_ref().map_or_else(
                                    || {
                                        tc.arguments.as_str().map_or_else(
                                            || tc.arguments.to_string(),
                                            str::to_string,
                                        )
                                    },
                                    Clone::clone,
                                );
                                input.push(serde_json::json!({
                                    "type": "custom_tool_call",
                                    "id": item_id,
                                    "call_id": tc.id,
                                    "name": tc.name,
                                    "input": raw_input,
                                }));
                            } else {
                                let args = tc
                                    .raw_arguments
                                    .as_ref()
                                    .map_or_else(|| tc.arguments.to_string(), Clone::clone);
                                input.push(serde_json::json!({
                                    "type": "function_call",
                                    "id": item_id,
                                    "call_id": tc.id,
                                    "name": tc.name,
                                    "arguments": args,
                                }));
                            }
                        }
                        ContentPart::Other { data, .. } if part.is_opaque_openai() => {
                            input.push(data.clone());
                        }
                        _ => {}
                    }
                }
            }
            Role::Tool => {
                for part in &msg.content {
                    if let ContentPart::ToolResult(tr) = part {
                        let output = tr
                            .content
                            .as_str()
                            .map_or_else(|| tr.content.to_string(), str::to_string);
                        let is_custom = custom_call_ids.contains(&tr.tool_call_id)
                            || msg.name.as_deref() == Some("apply_patch");
                        let mut item = if is_custom {
                            serde_json::json!({
                                "type": "custom_tool_call_output",
                                "call_id": tr.tool_call_id,
                                "output": output,
                            })
                        } else {
                            serde_json::json!({
                                "type": "function_call_output",
                                "call_id": tr.tool_call_id,
                                "output": output,
                            })
                        };
                        if tr.is_error && !is_custom {
                            item["status"] = serde_json::json!("incomplete");
                        }
                        input.push(item);
                    }
                }
            }
        }
    }

    let instructions = if instructions_parts.is_empty() {
        None
    } else {
        Some(instructions_parts.join("\n"))
    };

    (instructions, input)
}

/// Translate unified tool definitions to Responses API tool format.
pub(super) fn translate_tools(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            if t.is_custom() {
                serde_json::json!({
                    "type": "custom",
                    "name": t.name,
                    "description": t.description,
                    "format": t.custom_format().cloned().unwrap_or_else(|| serde_json::json!({})),
                })
            } else {
                serde_json::json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })
            }
        })
        .collect()
}

/// Translate unified `ToolChoice` to Responses API format.
fn translate_tool_choice(choice: &ToolChoice) -> serde_json::Value {
    match choice {
        ToolChoice::Auto => serde_json::json!("auto"),
        ToolChoice::None => serde_json::json!("none"),
        ToolChoice::Required => serde_json::json!("required"),
        ToolChoice::Named { tool_name } => {
            serde_json::json!({"type": "function", "name": tool_name})
        }
    }
}

/// Translate unified `ResponseFormat` to Responses API `text` field.
///
/// The Responses API uses `"text": {"format": {...}}` for structured output.
fn translate_response_format(format: &ResponseFormat) -> Option<serde_json::Value> {
    match format.kind {
        ResponseFormatType::Text => None,
        ResponseFormatType::JsonObject => {
            Some(serde_json::json!({"format": {"type": "json_object"}}))
        }
        ResponseFormatType::JsonSchema => {
            let mut schema_obj = serde_json::json!({
                "type": "json_schema",
                "name": "response",
                "strict": format.strict,
            });
            if let Some(schema) = &format.json_schema {
                schema_obj["schema"] = schema.clone();
            }
            Some(serde_json::json!({"format": schema_obj}))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::codec::CodecParams;
    use crate::types::{AudioData, DocumentData, ReasoningEffort, Request, ToolCall, ToolResult};

    fn minimal_request() -> Request {
        Request {
            model:            "gpt-4o".to_string(),
            messages:         vec![Message::user("Hello")],
            provider:         None,
            tools:            None,
            tool_choice:      None,
            response_format:  None,
            temperature:      None,
            top_p:            None,
            max_tokens:       None,
            stop_sequences:   None,
            reasoning_effort: None,
            speed:            None,
            metadata:         None,
            provider_options: None,
        }
    }

    /// Encode `request` (no catalog: the wire model id is the request model)
    /// and return the merged body, mirroring the adapter's encode path.
    fn encode_body(request: &Request, stream: bool, codex: bool) -> serde_json::Value {
        let params = CodecParams {
            openai_codex: codex,
            ..CodecParams::default()
        };
        let ctx = CodecCtx {
            request,
            provider_name: "openai",
            deployment_id: &request.model,
            model: None,
            params: &params,
        };
        encode(&ctx, stream).body
    }

    #[test]
    fn build_request_body_includes_metadata() {
        let mut metadata = HashMap::new();
        metadata.insert("user_id".to_string(), "u123".to_string());
        metadata.insert("session".to_string(), "s456".to_string());

        let mut request = minimal_request();
        request.metadata = Some(metadata);

        let body = encode_body(&request, false, false);
        let meta = body.get("metadata").expect("metadata should be present");
        assert_eq!(meta["user_id"], "u123");
        assert_eq!(meta["session"], "s456");
    }

    #[test]
    fn build_request_body_omits_metadata_when_none() {
        let request = minimal_request();
        let body = encode_body(&request, false, false);
        assert!(body.get("metadata").is_none());
    }

    #[test]
    fn build_request_body_merges_provider_options_openai() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "openai": {
                "store": true,
                "previous_response_id": "resp_abc123"
            }
        }));

        let body = encode_body(&request, false, false);
        assert_eq!(body["store"], true);
        assert_eq!(body["previous_response_id"], "resp_abc123");
    }

    #[test]
    fn build_request_body_provider_options_override_fields() {
        let mut request = minimal_request();
        request.temperature = Some(0.5);
        request.provider_options = Some(serde_json::json!({
            "openai": {
                "temperature": 0.9
            }
        }));

        let body = encode_body(&request, false, false);
        // provider_options should override the base field
        assert_eq!(body["temperature"], 0.9);
    }

    #[test]
    fn build_request_body_ignores_non_openai_provider_options() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "anthropic": {
                "thinking": {"type": "enabled", "budget_tokens": 10000}
            }
        }));

        let body = encode_body(&request, false, false);
        // anthropic options should not leak into the OpenAI request
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn build_request_body_no_provider_options() {
        let request = minimal_request();
        let body = encode_body(&request, false, false);
        assert_eq!(body["model"], "gpt-4o");
        // stream field is omitted when false (skip_serializing_if)
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn filter_input_tokens_request_body_keeps_only_count_fields() {
        let mut metadata = HashMap::new();
        metadata.insert("trace".to_string(), "abc".to_string());

        let mut request = minimal_request();
        request.tools = Some(vec![ToolDefinition::function(
            "search",
            "Search files",
            serde_json::json!({"type": "object"}),
        )]);
        request.reasoning_effort = Some(ReasoningEffort::Low);
        request.response_format = Some(ResponseFormat {
            kind:        ResponseFormatType::JsonSchema,
            json_schema: Some(serde_json::json!({"type": "object"})),
            strict:      true,
        });
        request.temperature = Some(0.2);
        request.top_p = Some(0.9);
        request.max_tokens = Some(32);
        request.stop_sequences = Some(vec!["END".to_string()]);
        request.metadata = Some(metadata);

        let body = encode_body(&request, true, false);
        let filtered = filter_input_tokens_request_body(body);

        assert_eq!(
            filtered,
            serde_json::json!({
                "input": [{"type": "message", "content": [{"text": "Hello", "type": "input_text"}], "role": "user"}],
                "model": "gpt-4o",
                "reasoning": {"effort": "low"},
                "text": {"format": {"name": "response", "schema": {"type": "object"}, "strict": true, "type": "json_schema"}},
                "tools": [{"description": "Search files", "name": "search", "parameters": {"type": "object"}, "type": "function"}]
            })
        );
        assert!(filtered.get("store").is_none());
        assert!(filtered.get("include").is_none());
        assert!(filtered.get("stream").is_none());
        assert!(filtered.get("max_output_tokens").is_none());
        assert!(filtered.get("metadata").is_none());
        assert!(filtered.get("temperature").is_none());
        assert!(filtered.get("top_p").is_none());
        assert!(filtered.get("stop").is_none());
    }

    #[test]
    fn filter_input_tokens_request_body_preserves_codex_serialization() {
        let body = encode_body(&minimal_request(), false, true);
        let filtered = filter_input_tokens_request_body(body);

        assert_eq!(filtered["instructions"], "");
        assert!(filtered.get("input").is_some());
        assert!(filtered.get("model").is_some());
        assert!(filtered.get("max_output_tokens").is_none());
        assert!(filtered.get("include").is_none());
    }

    #[test]
    fn count_tokens_endpoint_carries_filtered_body() {
        let request = minimal_request();
        let params = CodecParams::default();
        let ctx = CodecCtx {
            request:       &request,
            provider_name: "openai",
            deployment_id: &request.model,
            model:         None,
            params:        &params,
        };

        let encoded = encode_count_tokens(&ctx);
        assert_eq!(encoded.endpoint, "/responses/input_tokens");
        assert!(encoded.body.get("store").is_none());
        assert!(encoded.body.get("include").is_none());
        assert_eq!(encoded.body["model"], "gpt-4o");
    }

    #[test]
    fn build_request_body_includes_encrypted_reasoning_for_stateless_requests() {
        let request = minimal_request();

        let body = encode_body(&request, false, false);

        assert_eq!(
            body["include"],
            serde_json::json!(["reasoning.encrypted_content"])
        );
    }

    #[test]
    fn build_request_body_emits_custom_apply_patch_tool() {
        let mut request = minimal_request();
        request.tools = Some(vec![
            ToolDefinition::custom(
                "apply_patch",
                "Use the `apply_patch` tool to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.",
                serde_json::json!({
                    "type": "grammar",
                    "syntax": "lark",
                    "definition": "start: begin_patch hunk+ end_patch",
                }),
            ),
            ToolDefinition::function(
                "read_file",
                "Read file",
                serde_json::json!({
                    "type": "object",
                    "properties": {"file_path": {"type": "string"}},
                    "required": ["file_path"],
                }),
            ),
        ]);

        let body = encode_body(&request, false, false);
        let tools = body["tools"].as_array().expect("tools should be present");
        let apply_patch = tools
            .iter()
            .find(|tool| tool["name"] == "apply_patch")
            .expect("apply_patch tool should be present");
        let read_file = tools
            .iter()
            .find(|tool| tool["name"] == "read_file")
            .expect("read_file tool should be present");

        assert_eq!(apply_patch["type"], "custom");
        assert_eq!(apply_patch["format"]["type"], "grammar");
        assert_eq!(apply_patch["format"]["syntax"], "lark");
        assert!(apply_patch.get("parameters").is_none());
        assert_eq!(read_file["type"], "function");
        assert_eq!(read_file["parameters"]["type"], "object");
    }

    #[test]
    fn build_request_body_stream_flag() {
        let request = minimal_request();
        let body = encode_body(&request, true, false);
        assert!(body["stream"].as_bool().unwrap_or(false));
    }

    #[test]
    fn build_request_body_metadata_and_provider_options_together() {
        let mut metadata = HashMap::new();
        metadata.insert("trace_id".to_string(), "t789".to_string());

        let mut request = minimal_request();
        request.metadata = Some(metadata);
        request.provider_options = Some(serde_json::json!({
            "openai": {
                "store": true
            }
        }));

        let body = encode_body(&request, false, false);
        assert_eq!(body["metadata"]["trace_id"], "t789");
        assert_eq!(body["store"], true);
    }

    #[test]
    fn build_request_body_includes_stop_sequences() {
        let mut request = minimal_request();
        request.stop_sequences = Some(vec!["END".to_string(), "STOP".to_string()]);

        let body = encode_body(&request, false, false);
        let stop = body.get("stop").expect("stop should be present");
        let arr = stop.as_array().expect("stop should be an array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0], "END");
        assert_eq!(arr[1], "STOP");
    }

    #[test]
    fn build_request_body_omits_stop_when_none() {
        let request = minimal_request();
        let body = encode_body(&request, false, false);
        assert!(body.get("stop").is_none());
    }

    #[test]
    fn audio_content_produces_text_fallback() {
        let msg = Message {
            role:         Role::User,
            content:      vec![ContentPart::Audio(AudioData {
                url:        Some("https://example.com/audio.wav".to_string()),
                data:       None,
                media_type: None,
            })],
            name:         None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        let content = input[0]["content"]
            .as_array()
            .expect("content should be array");
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(
            content[0]["text"],
            "[Audio content not supported by this provider]"
        );
    }

    #[test]
    fn document_content_produces_text_fallback_with_filename() {
        let msg = Message {
            role:         Role::User,
            content:      vec![ContentPart::Document(DocumentData {
                url:        Some("https://example.com/doc.pdf".to_string()),
                data:       None,
                media_type: None,
                file_name:  Some("report.pdf".to_string()),
            })],
            name:         None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        let content = input[0]["content"]
            .as_array()
            .expect("content should be array");
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(
            content[0]["text"],
            "[Document 'report.pdf': content type not supported by this provider]"
        );
    }

    #[test]
    fn document_content_produces_text_fallback_without_filename() {
        let msg = Message {
            role:         Role::User,
            content:      vec![ContentPart::Document(DocumentData {
                url:        None,
                data:       Some(vec![1, 2, 3]),
                media_type: None,
                file_name:  None,
            })],
            name:         None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        let content = input[0]["content"]
            .as_array()
            .expect("content should be array");
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(
            content[0]["text"],
            "[Document content not supported by this provider]"
        );
    }

    #[test]
    fn translate_input_uses_item_id_for_id_field() {
        let mut tc = ToolCall::new(
            "call_xyz789",
            "get_weather",
            serde_json::json!({"location": "NYC"}),
        );
        tc.provider_metadata = Some(serde_json::json!({"id": "fc_abc123"}));

        let msg = Message {
            role:         Role::Assistant,
            content:      vec![ContentPart::ToolCall(tc)],
            name:         None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        let fc = &input[0];
        assert_eq!(fc["type"], "function_call");
        // id field uses the fc_ prefixed item ID
        assert_eq!(fc["id"], "fc_abc123");
        // call_id field uses the call_ prefixed call ID
        assert_eq!(fc["call_id"], "call_xyz789");
    }

    #[test]
    fn translate_input_falls_back_to_tc_id_without_metadata() {
        let tc = ToolCall::new("call_xyz789", "get_weather", serde_json::json!({}));

        let msg = Message {
            role:         Role::Assistant,
            content:      vec![ContentPart::ToolCall(tc)],
            name:         None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        let fc = &input[0];
        // Without provider_metadata, both fields use tc.id
        assert_eq!(fc["id"], "call_xyz789");
        assert_eq!(fc["call_id"], "call_xyz789");
    }

    #[test]
    fn reasoning_items_round_trip_through_translate_input() {
        let reasoning = serde_json::json!({
            "type": "reasoning",
            "id": "rs_abc123",
            "summary": [{"type": "summary_text", "text": "Thinking..."}]
        });
        let mut tc = ToolCall::new("call_789", "search", serde_json::json!({}));
        tc.provider_metadata = Some(serde_json::json!({"id": "fc_def456"}));

        let msg = Message {
            role:         Role::Assistant,
            content:      vec![
                ContentPart::Other {
                    kind: ContentPart::OPENAI_REASONING.to_string(),
                    data: reasoning,
                },
                ContentPart::ToolCall(tc),
            ],
            name:         None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        assert_eq!(input.len(), 2);
        // Reasoning item is emitted first
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["id"], "rs_abc123");
        // Function call follows
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["id"], "fc_def456");
        assert_eq!(input[1]["call_id"], "call_789");
    }

    #[test]
    fn reasoning_message_function_call_round_trip() {
        // Simulates an assistant turn with reasoning + text + tool call.
        // The opaque message item (with id/status) must be used instead of
        // constructing a new one from Text, so the reasoning item can find
        // its "required following item."
        let reasoning = serde_json::json!({
            "type": "reasoning",
            "id": "rs_xyz789",
            "summary": [{"type": "summary_text", "text": "Let me check..."}]
        });
        let opaque_message = serde_json::json!({
            "type": "message",
            "id": "msg_abc123",
            "status": "completed",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "Checking now."}]
        });
        let mut tc = ToolCall::new("call_001", "shell", serde_json::json!({"cmd": "ls"}));
        tc.provider_metadata = Some(serde_json::json!({"id": "fc_def456"}));

        let msg = Message {
            role:         Role::Assistant,
            content:      vec![
                ContentPart::Other {
                    kind: ContentPart::OPENAI_REASONING.to_string(),
                    data: reasoning,
                },
                ContentPart::Other {
                    kind: ContentPart::OPENAI_MESSAGE.to_string(),
                    data: opaque_message,
                },
                ContentPart::text("Checking now."),
                ContentPart::ToolCall(tc),
            ],
            name:         None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        assert_eq!(input.len(), 3);
        // Reasoning first
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["id"], "rs_xyz789");
        // Opaque message with id/status (not a reconstructed one)
        assert_eq!(input[1]["type"], "message");
        assert_eq!(input[1]["id"], "msg_abc123");
        assert_eq!(input[1]["status"], "completed");
        // Function call last
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["id"], "fc_def456");
    }

    #[test]
    fn text_without_opaque_message_still_constructs_message() {
        // For non-OpenAI turns or turns without preserved message items,
        // Text parts should still produce a constructed message.
        let msg = Message {
            role:         Role::Assistant,
            content:      vec![ContentPart::text("Hello")],
            name:         None,
            tool_call_id: None,
        };
        let (_, input) = translate_input(&[msg]);
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "assistant");
        // No id field on constructed messages
        assert!(input[0].get("id").is_none());
    }

    #[test]
    fn custom_tool_call_history_round_trips_through_translate_input() {
        let patch = "*** Begin Patch\n*** Delete File: stale.txt\n*** End Patch\n";
        let mut tc = ToolCall::new("call_001", "apply_patch", serde_json::json!(patch));
        tc.tool_type = "custom".to_string();
        tc.raw_arguments = Some(patch.to_string());
        tc.provider_metadata = Some(serde_json::json!({"id": "ctc_def456"}));

        let msg = Message {
            role:         Role::Assistant,
            content:      vec![ContentPart::ToolCall(tc)],
            name:         None,
            tool_call_id: None,
        };

        let (_, input) = translate_input(&[msg]);

        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "custom_tool_call");
        assert_eq!(input[0]["id"], "ctc_def456");
        assert_eq!(input[0]["call_id"], "call_001");
        assert_eq!(input[0]["name"], "apply_patch");
        assert_eq!(input[0]["input"], patch);
    }

    #[test]
    fn custom_tool_result_history_round_trips_through_translate_input() {
        let msg = Message {
            role:         Role::Tool,
            content:      vec![ContentPart::ToolResult(ToolResult::success(
                "call_001",
                serde_json::json!("Success. Updated the following files:\nA hello.txt\n"),
            ))],
            name:         Some("apply_patch".to_string()),
            tool_call_id: Some("call_001".to_string()),
        };

        let (_, input) = translate_input(&[msg]);

        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "custom_tool_call_output");
        assert_eq!(input[0]["call_id"], "call_001");
        assert_eq!(
            input[0]["output"],
            "Success. Updated the following files:\nA hello.txt\n"
        );
    }

    #[test]
    fn custom_tool_result_history_uses_prior_custom_call_without_tool_message_name() {
        let patch = "*** Begin Patch\n*** Add File: hello.txt\n+hello\n*** End Patch\n";
        let mut tc = ToolCall::new("call_001", "apply_patch", serde_json::json!(patch));
        tc.tool_type = "custom".to_string();
        tc.raw_arguments = Some(patch.to_string());
        tc.provider_metadata = Some(serde_json::json!({"id": "ctc_def456"}));

        let assistant_msg = Message {
            role:         Role::Assistant,
            content:      vec![ContentPart::ToolCall(tc)],
            name:         None,
            tool_call_id: None,
        };
        let tool_msg = Message::tool_result(
            "call_001",
            serde_json::json!("Success. Updated the following files:\nA hello.txt\n"),
            false,
        );

        let (_, input) = translate_input(&[assistant_msg, tool_msg]);

        assert_eq!(input.len(), 2);
        assert_eq!(input[1]["type"], "custom_tool_call_output");
        assert_eq!(input[1]["call_id"], "call_001");
        assert_eq!(
            input[1]["output"],
            "Success. Updated the following files:\nA hello.txt\n"
        );
    }
}
