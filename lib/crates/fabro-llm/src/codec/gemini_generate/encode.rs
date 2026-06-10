//! Request encoding: canonical request → Gemini `generateContent` body +
//! fully-formed endpoint (model-in-path, `?alt=sse` for streaming).
//!
//! Pure and sync. File-backed Image/Audio/Document attachments are resolved
//! to inline data by `attachments::resolve` in the adapter *before* encode
//! runs, so the content translation here never touches the filesystem.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use super::wire::{
    ApiRequest, Content, GeminiFunctionDecl, GeminiToolGroup, GenerationOptions, SystemInstruction,
};
use crate::codec::{CodecCtx, EncodedRequest};
use crate::providers::common::extract_system_prompt;
use crate::types::{
    ContentPart, Message, ResponseFormat, ResponseFormatType, Role, ToolChoice, ToolDefinition,
};

// --- Public entry points -----------------------------------------------------

pub(super) fn encode(ctx: &CodecCtx<'_>, stream: bool) -> EncodedRequest {
    let endpoint = if stream {
        format!(
            "/models/{}:streamGenerateContent?alt=sse",
            ctx.deployment_id
        )
    } else {
        format!("/models/{}:generateContent", ctx.deployment_id)
    };
    EncodedRequest {
        body: build_body(ctx),
        endpoint,
        headers: Vec::new(),
    }
}

pub(super) fn encode_count_tokens(ctx: &CodecCtx<'_>) -> EncodedRequest {
    EncodedRequest {
        body:     serde_json::json!({ "generateContentRequest": build_body(ctx) }),
        endpoint: format!("/models/{}:countTokens", ctx.deployment_id),
        headers:  Vec::new(),
    }
}

/// Build the Gemini API request body from the canonical request.
///
/// Returns a `serde_json::Value` so that `provider_options.gemini` fields can
/// be merged into the request before sending.
pub(super) fn build_body(ctx: &CodecCtx<'_>) -> serde_json::Value {
    let request = ctx.request;
    let (system_text, other_messages) = extract_system_prompt(&request.messages);

    let system_instruction = system_text.map(|text| SystemInstruction {
        parts: vec![serde_json::json!({"text": text})],
    });

    let contents = translate_messages(&other_messages);

    let (response_mime_type, response_schema) = request
        .response_format
        .as_ref()
        .map_or((None, None), translate_response_format);

    let generation_config = GenerationOptions {
        temperature: request.temperature,
        max_output_tokens: request.max_tokens,
        top_p: request.top_p,
        stop_sequences: request.stop_sequences.clone(),
        response_mime_type,
        response_schema,
    };

    let api_tools = request.tools.as_ref().map(|t| translate_tools(t));
    let tool_config = request.tool_choice.as_ref().map(translate_tool_choice);

    let api_request = ApiRequest {
        contents,
        system_instruction,
        generation_config: Some(generation_config),
        tools: api_tools,
        tool_config,
    };

    let mut body = serde_json::to_value(&api_request).unwrap_or_default();
    merge_provider_options(&mut body, request.provider_options.as_ref());
    apply_default_safety_settings(&mut body);
    body
}

// --- Content / message / tool translation ------------------------------------

/// Build a mapping from tool call ID to function name by scanning assistant
/// messages.
///
/// Gemini uses function names (not call IDs) in `functionResponse`. Since the
/// decoder generates synthetic UUIDs as tool call IDs, we need this mapping to
/// recover the original function name when sending tool results back.
fn build_tool_call_id_to_name(messages: &[&Message]) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for msg in messages {
        if msg.role == Role::Assistant {
            for part in &msg.content {
                if let ContentPart::ToolCall(tc) = part {
                    map.insert(tc.id.clone(), tc.name.clone());
                }
            }
        }
    }
    map
}

/// Translate unified messages to Gemini content format. Sync: file-backed
/// attachments are already resolved to inline data upstream.
pub(super) fn translate_messages(messages: &[&Message]) -> Vec<Content> {
    let id_to_name = build_tool_call_id_to_name(messages);
    let mut contents: Vec<Content> = Vec::new();

    for msg in messages {
        let role = match msg.role {
            Role::Assistant => "model",
            Role::User | Role::Tool => "user",
            Role::System | Role::Developer => continue,
        };

        let mut parts = Vec::new();
        for part in &msg.content {
            let maybe_part = match part {
                ContentPart::Text(text) => Some(serde_json::json!({"text": text})),
                ContentPart::ToolCall(tc) => {
                    let mut part_json = serde_json::json!({
                        "functionCall": {
                            "name": tc.name,
                            "args": tc.arguments,
                        }
                    });
                    // Re-attach thought_signature as sibling of functionCall
                    if let Some(sig) = tc
                        .provider_metadata
                        .as_ref()
                        .and_then(|m| m.get("thoughtSignature"))
                    {
                        part_json["thoughtSignature"] = sig.clone();
                    }
                    Some(part_json)
                }
                ContentPart::Image(img) => match &img.url {
                    Some(url) => {
                        let mime = img.media_type.as_deref().unwrap_or("image/png");
                        Some(serde_json::json!({
                            "fileData": {"mimeType": mime, "fileUri": url}
                        }))
                    }
                    None => img.data.as_ref().map(|data| {
                        let mime = img.media_type.as_deref().unwrap_or("image/png");
                        let b64 = BASE64_STANDARD.encode(data);
                        serde_json::json!({"inlineData": {"mimeType": mime, "data": b64}})
                    }),
                },
                ContentPart::Audio(audio) => match &audio.url {
                    Some(url) => {
                        let mime = audio.media_type.as_deref().unwrap_or("audio/wav");
                        Some(serde_json::json!({
                            "fileData": {"mimeType": mime, "fileUri": url}
                        }))
                    }
                    None => audio.data.as_ref().map(|data| {
                        let mime = audio.media_type.as_deref().unwrap_or("audio/wav");
                        let b64 = BASE64_STANDARD.encode(data);
                        serde_json::json!({"inlineData": {"mimeType": mime, "data": b64}})
                    }),
                },
                ContentPart::Document(doc) => match &doc.url {
                    Some(url) => {
                        let mime = doc.media_type.as_deref().unwrap_or("application/pdf");
                        Some(serde_json::json!({
                            "fileData": {"mimeType": mime, "fileUri": url}
                        }))
                    }
                    None => doc.data.as_ref().map(|data| {
                        let mime = doc.media_type.as_deref().unwrap_or("application/pdf");
                        let b64 = BASE64_STANDARD.encode(data);
                        serde_json::json!({"inlineData": {"mimeType": mime, "data": b64}})
                    }),
                },
                ContentPart::ToolResult(tr) => {
                    // Gemini's functionResponse uses the function *name*, not the call ID.
                    // Look up the original function name from the tool call mapping.
                    let function_name = id_to_name
                        .get(&tr.tool_call_id)
                        .cloned()
                        .unwrap_or_else(|| tr.tool_call_id.clone());
                    let response = tr.content.as_str().map_or_else(
                        || {
                            if tr.content.is_object() {
                                tr.content.clone()
                            } else {
                                serde_json::json!({"result": tr.content.to_string()})
                            }
                        },
                        |s| serde_json::json!({"result": s}),
                    );
                    Some(serde_json::json!({
                        "functionResponse": {
                            "name": function_name,
                            "response": response,
                        }
                    }))
                }
                _ => None,
            };
            if let Some(part_json) = maybe_part {
                parts.push(part_json);
            }
        }

        if parts.is_empty() {
            continue;
        }

        contents.push(Content {
            role: role.to_string(),
            parts,
        });
    }

    contents
}

/// Translate unified tool definitions to Gemini's format.
fn translate_tools(tools: &[ToolDefinition]) -> Vec<GeminiToolGroup> {
    vec![GeminiToolGroup {
        function_declarations: tools
            .iter()
            .map(|t| GeminiFunctionDecl {
                name:        t.name.clone(),
                description: t.description.clone(),
                parameters:  t.parameters.clone(),
            })
            .collect(),
    }]
}

/// Translate unified `ToolChoice` to Gemini's `toolConfig`.
fn translate_tool_choice(choice: &ToolChoice) -> serde_json::Value {
    match choice {
        ToolChoice::Auto => serde_json::json!({
            "functionCallingConfig": {"mode": "AUTO"}
        }),
        ToolChoice::None => serde_json::json!({
            "functionCallingConfig": {"mode": "NONE"}
        }),
        ToolChoice::Required => serde_json::json!({
            "functionCallingConfig": {"mode": "ANY"}
        }),
        ToolChoice::Named { tool_name } => serde_json::json!({
            "functionCallingConfig": {
                "mode": "ANY",
                "allowedFunctionNames": [tool_name],
            }
        }),
    }
}

/// Translate unified `ResponseFormat` to Gemini generation config fields.
///
/// Returns `(response_mime_type, response_schema)`.
fn translate_response_format(
    format: &ResponseFormat,
) -> (Option<String>, Option<serde_json::Value>) {
    match format.kind {
        ResponseFormatType::Text => (None, None),
        ResponseFormatType::JsonObject => (Some("application/json".to_string()), None),
        ResponseFormatType::JsonSchema => (
            Some("application/json".to_string()),
            format.json_schema.clone(),
        ),
    }
}

/// Merge `provider_options.gemini` fields into the serialized API request body.
///
/// Known fields like `safety_settings` and `cached_content` are set directly.
/// Any other fields are merged at the top level, allowing pass-through of
/// Gemini-specific options not covered by the unified schema.
fn merge_provider_options(
    body: &mut serde_json::Value,
    provider_options: Option<&serde_json::Value>,
) {
    let Some(gemini_opts) = provider_options.and_then(|opts| opts.get("gemini")) else {
        return;
    };
    let Some(body_map) = body.as_object_mut() else {
        return;
    };
    let Some(gemini_map) = gemini_opts.as_object() else {
        return;
    };

    for (key, value) in gemini_map {
        body_map.insert(key.clone(), value.clone());
    }
}

/// Apply default safety settings if none were provided via provider_options.
fn apply_default_safety_settings(body: &mut serde_json::Value) {
    if body.get("safety_settings").is_some() {
        return;
    }
    if let Some(body_map) = body.as_object_mut() {
        body_map.insert(
            "safety_settings".to_string(),
            serde_json::json!([{
                "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
                "threshold": "BLOCK_ONLY_HIGH"
            }]),
        );
    }
}
