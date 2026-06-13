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
use crate::codec::{CodecCtx, EncodedRequest, extract_system_prompt};
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

/// Encode a media attachment part: URL-backed attachments become `fileData`,
/// inline bytes become base64 `inlineData`.
fn media_part(
    url: Option<&str>,
    data: Option<&[u8]>,
    media_type: Option<&str>,
    default_mime: &str,
) -> Option<serde_json::Value> {
    let mime = media_type.unwrap_or(default_mime);
    match url {
        Some(url) => Some(serde_json::json!({
            "fileData": {"mimeType": mime, "fileUri": url}
        })),
        None => data.map(|data| {
            let b64 = BASE64_STANDARD.encode(data);
            serde_json::json!({"inlineData": {"mimeType": mime, "data": b64}})
        }),
    }
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
                ContentPart::Image(img) => media_part(
                    img.url.as_deref(),
                    img.data.as_deref(),
                    img.media_type.as_deref(),
                    "image/png",
                ),
                ContentPart::Audio(audio) => media_part(
                    audio.url.as_deref(),
                    audio.data.as_deref(),
                    audio.media_type.as_deref(),
                    "audio/wav",
                ),
                ContentPart::Document(doc) => media_part(
                    doc.url.as_deref(),
                    doc.data.as_deref(),
                    doc.media_type.as_deref(),
                    "application/pdf",
                ),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::CodecParams;
    use crate::types::{AudioData, DocumentData, Request, ToolCall};

    fn minimal_request() -> Request {
        Request {
            model:            "gemini-2.0-flash".to_string(),
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

    /// Build the request body the way the adapter's encode path does (no
    /// catalog: the wire model id is the request model).
    fn body_for(request: &Request) -> serde_json::Value {
        let params = CodecParams::default();
        let ctx = CodecCtx {
            request,
            provider_name: "gemini",
            deployment_id: &request.model,
            model: None,
            params: &params,
        };
        build_body(&ctx)
    }

    #[test]
    fn provider_options_none_produces_standard_body() {
        let request = minimal_request();
        let body = body_for(&request);
        assert!(body.get("safetySettings").is_none());
        assert!(body.get("cachedContent").is_none());
    }

    #[test]
    fn encode_endpoints_carry_model_and_streaming_variant() {
        let request = minimal_request();
        let params = CodecParams::default();
        let ctx = CodecCtx {
            request:       &request,
            provider_name: "gemini",
            deployment_id: &request.model,
            model:         None,
            params:        &params,
        };

        assert_eq!(
            encode(&ctx, false).endpoint,
            "/models/gemini-2.0-flash:generateContent"
        );
        assert_eq!(
            encode(&ctx, true).endpoint,
            "/models/gemini-2.0-flash:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            encode_count_tokens(&ctx).endpoint,
            "/models/gemini-2.0-flash:countTokens"
        );
    }

    #[test]
    fn count_tokens_body_uses_only_generate_content_request_top_level() {
        let mut request = minimal_request();
        request.tools = Some(vec![ToolDefinition::function(
            "search",
            "Search files",
            serde_json::json!({"type": "object"}),
        )]);
        let params = CodecParams::default();
        let ctx = CodecCtx {
            request:       &request,
            provider_name: "gemini",
            deployment_id: &request.model,
            model:         None,
            params:        &params,
        };
        let count_body = encode_count_tokens(&ctx).body;

        assert!(count_body.get("generateContentRequest").is_some());
        assert!(count_body.get("contents").is_none());
        assert!(
            count_body["generateContentRequest"]
                .get("contents")
                .is_some()
        );
        assert!(count_body["generateContentRequest"].get("tools").is_some());
    }

    #[test]
    fn provider_options_gemini_safety_settings_merged() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "gemini": {
                "safetySettings": [
                    {"category": "HARM_CATEGORY_HARASSMENT", "threshold": "BLOCK_NONE"}
                ]
            }
        }));

        let body = body_for(&request);
        let safety = body
            .get("safetySettings")
            .expect("safetySettings should be present");
        let arr = safety.as_array().expect("should be an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["category"], "HARM_CATEGORY_HARASSMENT");
    }

    #[test]
    fn provider_options_gemini_cached_content_merged() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "gemini": {
                "cachedContent": "projects/my-project/cachedContents/abc123"
            }
        }));

        let body = body_for(&request);
        assert_eq!(
            body.get("cachedContent")
                .and_then(serde_json::Value::as_str),
            Some("projects/my-project/cachedContents/abc123")
        );
    }

    #[test]
    fn provider_options_gemini_multiple_fields_merged() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "gemini": {
                "safetySettings": [{"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "BLOCK_LOW_AND_ABOVE"}],
                "cachedContent": "cache-id",
                "customField": "custom-value"
            }
        }));

        let body = body_for(&request);
        assert!(body.get("safetySettings").is_some());
        assert_eq!(
            body.get("cachedContent")
                .and_then(serde_json::Value::as_str),
            Some("cache-id")
        );
        assert_eq!(
            body.get("customField").and_then(serde_json::Value::as_str),
            Some("custom-value")
        );
    }

    #[test]
    fn provider_options_other_provider_ignored() {
        let mut request = minimal_request();
        request.provider_options = Some(serde_json::json!({
            "anthropic": {
                "auto_cache": false
            }
        }));

        let body = body_for(&request);
        assert!(body.get("auto_cache").is_none());
    }

    #[test]
    fn provider_options_gemini_preserves_standard_fields() {
        let mut request = minimal_request();
        request.temperature = Some(0.5);
        request.max_tokens = Some(100);
        request.provider_options = Some(serde_json::json!({
            "gemini": {
                "cachedContent": "cache-id"
            }
        }));

        let body = body_for(&request);
        let gen_config = body
            .get("generationConfig")
            .expect("generationConfig should exist");
        assert_eq!(
            gen_config
                .get("temperature")
                .and_then(serde_json::Value::as_f64),
            Some(0.5)
        );
        assert_eq!(
            gen_config
                .get("maxOutputTokens")
                .and_then(serde_json::Value::as_i64),
            Some(100)
        );
        assert_eq!(
            body.get("cachedContent")
                .and_then(serde_json::Value::as_str),
            Some("cache-id")
        );
    }

    #[test]
    fn merge_provider_options_with_non_object_gemini_value() {
        let mut body = serde_json::json!({"contents": []});
        let opts = serde_json::json!({"gemini": "not-an-object"});
        merge_provider_options(&mut body, Some(&opts));
        // Should not crash and body should be unchanged
        assert!(body.get("contents").is_some());
    }

    #[test]
    fn audio_url_translates_to_file_data() {
        let msg = Message {
            role:         Role::User,
            content:      vec![ContentPart::Audio(AudioData {
                url:        Some("https://example.com/audio.wav".to_string()),
                data:       None,
                media_type: Some("audio/wav".to_string()),
            })],
            name:         None,
            tool_call_id: None,
        };
        let contents = translate_messages(&[&msg]);
        assert_eq!(contents.len(), 1);
        let part = &contents[0].parts[0];
        assert_eq!(part["fileData"]["mimeType"], "audio/wav");
        assert_eq!(part["fileData"]["fileUri"], "https://example.com/audio.wav");
    }

    #[test]
    fn audio_base64_translates_to_inline_data() {
        let msg = Message {
            role:         Role::User,
            content:      vec![ContentPart::Audio(AudioData {
                url:        None,
                data:       Some(vec![0xFF, 0xFB, 0x90]),
                media_type: None,
            })],
            name:         None,
            tool_call_id: None,
        };
        let contents = translate_messages(&[&msg]);
        let part = &contents[0].parts[0];
        assert_eq!(part["inlineData"]["mimeType"], "audio/wav");
        assert!(part["inlineData"]["data"].as_str().is_some());
    }

    #[test]
    fn document_url_translates_to_file_data() {
        let msg = Message {
            role:         Role::User,
            content:      vec![ContentPart::Document(DocumentData {
                url:        Some("https://example.com/doc.pdf".to_string()),
                data:       None,
                media_type: Some("application/pdf".to_string()),
                file_name:  Some("doc.pdf".to_string()),
            })],
            name:         None,
            tool_call_id: None,
        };
        let contents = translate_messages(&[&msg]);
        let part = &contents[0].parts[0];
        assert_eq!(part["fileData"]["mimeType"], "application/pdf");
        assert_eq!(part["fileData"]["fileUri"], "https://example.com/doc.pdf");
    }

    #[test]
    fn document_base64_translates_to_inline_data() {
        let msg = Message {
            role:         Role::User,
            content:      vec![ContentPart::Document(DocumentData {
                url:        None,
                data:       Some(vec![0x25, 0x50, 0x44, 0x46]),
                media_type: None,
                file_name:  None,
            })],
            name:         None,
            tool_call_id: None,
        };
        let contents = translate_messages(&[&msg]);
        let part = &contents[0].parts[0];
        assert_eq!(part["inlineData"]["mimeType"], "application/pdf");
        assert!(part["inlineData"]["data"].as_str().is_some());
    }

    #[test]
    fn translate_messages_function_call_includes_thought_signature() {
        let mut tc = ToolCall::new(
            "call-1",
            "get_weather",
            serde_json::json!({"location": "NYC"}),
        );
        tc.provider_metadata = Some(serde_json::json!({"thoughtSignature": "sig456"}));

        let msg = Message {
            role:         Role::Assistant,
            content:      vec![ContentPart::ToolCall(tc)],
            name:         None,
            tool_call_id: None,
        };
        let contents = translate_messages(&[&msg]);
        assert_eq!(contents.len(), 1);

        let part = &contents[0].parts[0];
        assert!(part.get("functionCall").is_some());
        assert_eq!(part["thoughtSignature"], "sig456");
    }

    #[test]
    fn translate_messages_function_call_without_thought_signature() {
        let tc = ToolCall::new(
            "call-1",
            "get_weather",
            serde_json::json!({"location": "NYC"}),
        );

        let msg = Message {
            role:         Role::Assistant,
            content:      vec![ContentPart::ToolCall(tc)],
            name:         None,
            tool_call_id: None,
        };
        let contents = translate_messages(&[&msg]);
        assert_eq!(contents.len(), 1);

        let part = &contents[0].parts[0];
        assert!(part.get("functionCall").is_some());
        assert!(part.get("thoughtSignature").is_none());
    }
}
