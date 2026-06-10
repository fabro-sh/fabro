//! Request encoding: canonical request → OpenAI Responses API body.
//!
//! Pure and sync. File-backed image attachments are resolved to inline data by
//! `attachments::resolve` in the adapter *before* encode runs, so the content
//! translation here never touches the filesystem.

use std::collections::HashMap;

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
    let body = build_body(ctx, false);
    EncodedRequest {
        body:     filter_input_tokens_request_body(&body),
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
fn filter_input_tokens_request_body(body: &serde_json::Value) -> serde_json::Value {
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

    let Some(source) = body.as_object() else {
        return serde_json::json!({});
    };

    let mut filtered = serde_json::Map::new();
    for field in ALLOWED_FIELDS {
        if let Some(value) = source.get(*field) {
            filtered.insert((*field).to_string(), value.clone());
        }
    }
    serde_json::Value::Object(filtered)
}

// --- Content / message / tool translation ------------------------------------

/// Translate unified messages to Responses API `input` array format. Sync:
/// file-backed image attachments are already resolved to inline data upstream.
pub(super) fn translate_input(messages: &[Message]) -> (Option<String>, Vec<serde_json::Value>) {
    let mut instructions_parts: Vec<String> = Vec::new();
    let mut input: Vec<serde_json::Value> = Vec::new();
    let mut tool_call_types: HashMap<String, (String, String)> = HashMap::new();

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
                            tool_call_types
                                .insert(tc.id.clone(), (tc.tool_type.clone(), tc.name.clone()));
                            if tc.tool_type == "custom" {
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
                        let is_custom = tool_call_types
                            .get(&tr.tool_call_id)
                            .is_some_and(|(tool_type, _)| tool_type == "custom")
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
