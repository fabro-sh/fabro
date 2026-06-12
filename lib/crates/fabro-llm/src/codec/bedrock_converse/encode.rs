//! Request encoding: canonical `Request` → Converse envelope.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Map, Value, json};

use crate::codec::{CodecCtx, EncodedRequest, extract_system_prompt};
use crate::error::Error;
use crate::types::{ContentPart, Message, Request, Role, ToolChoice};

pub(super) fn encode(ctx: &CodecCtx<'_>, stream: bool) -> Result<EncodedRequest, Error> {
    let request = ctx.request;
    if request.response_format.is_some() {
        return Err(Error::Configuration {
            message: format!(
                "provider '{}' does not support response_format yet (Bedrock Converse \
                 structured output is a named follow-up)",
                ctx.provider_name
            ),
            source:  None,
        });
    }

    let caching = supports_prompt_cache(ctx);
    let (system, conversation) = extract_system_prompt(&request.messages);

    let mut body = Map::new();

    if let Some(system) = system {
        let mut blocks = vec![json!({ "text": system })];
        if caching {
            blocks.push(cache_point());
        }
        body.insert("system".to_string(), Value::Array(blocks));
    }

    let mut messages = Vec::new();
    for message in conversation {
        if let Some(value) = encode_message(message) {
            messages.push(value);
        }
    }
    if caching {
        apply_cache_point_to_conversation_prefix(&mut messages);
    }
    body.insert("messages".to_string(), Value::Array(messages));

    let mut inference = Map::new();
    if let Some(max_tokens) = request.max_tokens {
        inference.insert("maxTokens".to_string(), json!(max_tokens));
    }
    if let Some(temperature) = request.temperature {
        inference.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = request.top_p {
        inference.insert("topP".to_string(), json!(top_p));
    }
    if let Some(stop) = &request.stop_sequences {
        if !stop.is_empty() {
            inference.insert("stopSequences".to_string(), json!(stop));
        }
    }
    if !inference.is_empty() {
        body.insert("inferenceConfig".to_string(), Value::Object(inference));
    }

    if let Some(tool_config) = encode_tool_config(request, caching) {
        body.insert("toolConfig".to_string(), tool_config);
    }

    let mut body = Value::Object(body);
    merge_provider_options(
        &mut body,
        request.provider_options.as_ref(),
        ctx.provider_name,
    );

    let action = if stream {
        "converse-stream"
    } else {
        "converse"
    };
    Ok(EncodedRequest {
        body,
        endpoint: format!("/model/{}/{action}", ctx.deployment_id),
        headers: Vec::new(),
    })
}

fn supports_prompt_cache(ctx: &CodecCtx<'_>) -> bool {
    ctx.model.is_some_and(|m| m.features.prompt_cache)
}

fn cache_point() -> Value {
    json!({ "cachePoint": { "type": "default" } })
}

/// Encode one conversation message. Tool-role messages carry their results in
/// user-role messages (Converse has no tool role). Returns `None` when no
/// block survives translation.
fn encode_message(message: &Message) -> Option<Value> {
    let role = match message.role {
        Role::Assistant => "assistant",
        // Tool results ride in user messages on the Converse wire.
        _ => "user",
    };

    let mut blocks: Vec<Value> = message
        .content
        .iter()
        .filter_map(encode_content_part)
        .collect();

    // Tool-role messages whose result lives on the message rather than in a
    // ToolResult part.
    if blocks.is_empty() && message.role == Role::Tool {
        if let Some(tool_call_id) = &message.tool_call_id {
            let text = message.text();
            blocks.push(json!({
                "toolResult": {
                    "toolUseId": tool_call_id,
                    "content": [{ "text": text }],
                }
            }));
        }
    }

    if blocks.is_empty() {
        return None;
    }
    Some(json!({ "role": role, "content": blocks }))
}

fn encode_content_part(part: &ContentPart) -> Option<Value> {
    match part {
        ContentPart::Text(text) => {
            if text.is_empty() {
                None
            } else {
                Some(json!({ "text": text }))
            }
        }
        // Converse has no URL sources; the adapter's attachment resolution
        // inlines file-backed parts ahead of encoding, and URL-only parts are
        // dropped (the established drop-don't-fail attachment contract).
        ContentPart::Image(image) => {
            let bytes = image.data.as_ref()?;
            Some(json!({
                "image": {
                    "format": media_format(image.media_type.as_deref(), "png"),
                    "source": { "bytes": BASE64.encode(bytes) },
                }
            }))
        }
        ContentPart::Document(document) => {
            let bytes = document.data.as_ref()?;
            Some(json!({
                "document": {
                    "format": media_format(document.media_type.as_deref(), "pdf"),
                    "name": document.file_name.as_deref().unwrap_or("document"),
                    "source": { "bytes": BASE64.encode(bytes) },
                }
            }))
        }
        ContentPart::ToolCall(tool_call) => Some(json!({
            "toolUse": {
                "toolUseId": tool_call.id,
                "name": tool_call.name,
                "input": tool_call.arguments,
            }
        })),
        ContentPart::ToolResult(result) => {
            let content = match &result.content {
                Value::String(text) => json!([{ "text": text }]),
                other => json!([{ "json": other }]),
            };
            let mut block = Map::new();
            block.insert("toolUseId".to_string(), json!(result.tool_call_id));
            block.insert("content".to_string(), content);
            if result.is_error {
                block.insert("status".to_string(), json!("error"));
            }
            Some(json!({ "toolResult": Value::Object(block) }))
        }
        ContentPart::Thinking(thinking) => {
            if thinking.redacted {
                Some(json!({
                    "reasoningContent": { "redactedContent": thinking.text }
                }))
            } else {
                let mut text_block = Map::new();
                text_block.insert("text".to_string(), json!(thinking.text));
                if let Some(signature) = &thinking.signature {
                    // Echoed back unmodified — Bedrock validates it.
                    text_block.insert("signature".to_string(), json!(signature));
                }
                Some(json!({
                    "reasoningContent": { "reasoningText": Value::Object(text_block) }
                }))
            }
        }
        // Audio input and opaque foreign parts have no Converse encoding.
        ContentPart::Audio(_) | ContentPart::Other { .. } => None,
    }
}

/// `image/png` → `png`; missing/odd media types fall back to `default`.
fn media_format(media_type: Option<&str>, default: &str) -> String {
    media_type
        .and_then(|m| m.split('/').next_back())
        .filter(|s| !s.is_empty())
        .unwrap_or(default)
        .to_string()
}

fn encode_tool_config(request: &Request, caching: bool) -> Option<Value> {
    let tools = request.tools.as_ref()?;
    if tools.is_empty() {
        return None;
    }
    // `tool_choice: none` is rejected at the adapter's validate_request;
    // defensively drop the toolConfig if it slips through.
    if request.tool_choice == Some(ToolChoice::None) {
        return None;
    }

    let mut entries: Vec<Value> = tools
        .iter()
        .map(|tool| {
            json!({
                "toolSpec": {
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": { "json": tool.parameters },
                }
            })
        })
        .collect();
    if caching {
        entries.push(cache_point());
    }

    let mut config = Map::new();
    config.insert("tools".to_string(), Value::Array(entries));
    match &request.tool_choice {
        Some(ToolChoice::Required) => {
            config.insert("toolChoice".to_string(), json!({ "any": {} }));
        }
        Some(ToolChoice::Named { tool_name }) => {
            config.insert(
                "toolChoice".to_string(),
                json!({ "tool": { "name": tool_name } }),
            );
        }
        // Auto is the wire default; ToolChoice::None dropped the config above.
        Some(ToolChoice::Auto | ToolChoice::None) | None => {}
    }
    Some(Value::Object(config))
}

/// Mirror the anthropic codec's conversation-prefix cache placement: a
/// `cachePoint` at the end of the second-to-last user message, so the prior
/// turns stay cached while the newest turn streams.
fn apply_cache_point_to_conversation_prefix(messages: &mut [Value]) {
    let user_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.get("role").and_then(Value::as_str) == Some("user"))
        .map(|(i, _)| i)
        .collect();

    if user_indices.len() < 2 {
        return;
    }

    let target = user_indices[user_indices.len() - 2];
    if let Some(content) = messages[target]
        .get_mut("content")
        .and_then(Value::as_array_mut)
    {
        content.push(cache_point());
    }
}

/// Merge `provider_options.<provider_name>` keys into the top level of the
/// body (the same adapter-name-keyed contract as the openai_compatible
/// codec). This is the passthrough for `additionalModelRequestFields`,
/// `guardrailConfig`, `serviceTier`, and other Converse extensions.
fn merge_provider_options(body: &mut Value, provider_options: Option<&Value>, provider_name: &str) {
    let Some(opts) = provider_options.and_then(|opts| opts.get(provider_name)) else {
        return;
    };
    let Some(body_map) = body.as_object_mut() else {
        return;
    };
    let Some(opts_map) = opts.as_object() else {
        return;
    };
    for (key, value) in opts_map {
        body_map.insert(key.clone(), value.clone());
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::codec::CodecParams;
    use crate::types::{
        ResponseFormat, ResponseFormatType, ThinkingData, ToolDefinition, ToolResult,
    };

    fn base_request(model: &str) -> Request {
        Request {
            model:            model.to_string(),
            messages:         vec![Message::user("Hello")],
            provider:         Some("bedrock".to_string()),
            tools:            None,
            tool_choice:      None,
            response_format:  None,
            temperature:      Some(0.5),
            top_p:            None,
            max_tokens:       Some(256),
            stop_sequences:   None,
            reasoning_effort: None,
            speed:            None,
            metadata:         None,
            provider_options: None,
        }
    }

    fn encode_with(request: &Request) -> EncodedRequest {
        let params = CodecParams::default();
        let ctx = CodecCtx {
            request,
            provider_name: "bedrock",
            deployment_id: "us.anthropic.claude-sonnet-4-6",
            model: None,
            params: &params,
        };
        encode(&ctx, false).unwrap()
    }

    #[test]
    fn endpoint_carries_model_and_action() {
        let request = base_request("claude");
        let params = CodecParams::default();
        let ctx = CodecCtx {
            request:       &request,
            provider_name: "bedrock",
            deployment_id: "us.anthropic.claude-sonnet-4-6",
            model:         None,
            params:        &params,
        };
        assert_eq!(
            encode(&ctx, false).unwrap().endpoint,
            "/model/us.anthropic.claude-sonnet-4-6/converse"
        );
        assert_eq!(
            encode(&ctx, true).unwrap().endpoint,
            "/model/us.anthropic.claude-sonnet-4-6/converse-stream"
        );
    }

    #[test]
    fn system_messages_become_top_level_system_blocks() {
        let mut request = base_request("claude");
        request.messages = vec![Message::system("Be brief"), Message::user("Hi")];
        let encoded = encode_with(&request);
        assert_eq!(encoded.body["system"][0]["text"], "Be brief");
        assert_eq!(encoded.body["messages"][0]["role"], "user");
        assert_eq!(encoded.body["messages"][0]["content"][0]["text"], "Hi");
    }

    #[test]
    fn inference_config_uses_camel_case() {
        let encoded = encode_with(&base_request("claude"));
        assert_eq!(encoded.body["inferenceConfig"]["maxTokens"], 256);
        assert_eq!(encoded.body["inferenceConfig"]["temperature"], 0.5);
    }

    #[test]
    fn tools_encode_as_tool_specs_with_choice() {
        let mut request = base_request("claude");
        request.tools = Some(vec![ToolDefinition::function(
            "search",
            "Search things",
            json!({"type": "object"}),
        )]);
        request.tool_choice = Some(ToolChoice::named("search"));
        let encoded = encode_with(&request);
        let spec = &encoded.body["toolConfig"]["tools"][0]["toolSpec"];
        assert_eq!(spec["name"], "search");
        assert_eq!(spec["inputSchema"]["json"]["type"], "object");
        assert_eq!(
            encoded.body["toolConfig"]["toolChoice"]["tool"]["name"],
            "search"
        );
    }

    #[test]
    fn tool_results_ride_in_user_messages() {
        let mut request = base_request("claude");
        request.messages = vec![Message {
            role:         Role::Tool,
            content:      vec![ContentPart::ToolResult(ToolResult {
                tool_call_id:     "tool-1".to_string(),
                content:          json!("42"),
                is_error:         false,
                image_data:       None,
                image_media_type: None,
            })],
            name:         None,
            tool_call_id: Some("tool-1".to_string()),
        }];
        let encoded = encode_with(&request);
        let message = &encoded.body["messages"][0];
        assert_eq!(message["role"], "user");
        assert_eq!(message["content"][0]["toolResult"]["toolUseId"], "tool-1");
        assert_eq!(
            message["content"][0]["toolResult"]["content"][0]["text"],
            "42"
        );
    }

    #[test]
    fn thinking_parts_restructure_into_reasoning_text_blocks() {
        let mut request = base_request("claude");
        request.messages = vec![Message {
            role:         Role::Assistant,
            content:      vec![ContentPart::Thinking(ThinkingData {
                text:      "prior thoughts".to_string(),
                signature: Some("sig-1".to_string()),
                redacted:  false,
            })],
            name:         None,
            tool_call_id: None,
        }];
        let encoded = encode_with(&request);
        let block = &encoded.body["messages"][0]["content"][0]["reasoningContent"]["reasoningText"];
        assert_eq!(block["text"], "prior thoughts");
        assert_eq!(block["signature"], "sig-1");
    }

    #[test]
    fn provider_options_merge_top_level() {
        let mut request = base_request("claude");
        request.provider_options = Some(json!({
            "bedrock": {
                "additionalModelRequestFields": {"top_k": 200},
                "serviceTier": {"type": "flex"}
            }
        }));
        let encoded = encode_with(&request);
        assert_eq!(encoded.body["additionalModelRequestFields"]["top_k"], 200);
        assert_eq!(encoded.body["serviceTier"]["type"], "flex");
    }

    #[test]
    fn response_format_is_rejected() {
        let mut request = base_request("claude");
        request.response_format = Some(ResponseFormat {
            kind:        ResponseFormatType::JsonSchema,
            json_schema: Some(json!({"type": "object"})),
            strict:      false,
        });
        let params = CodecParams::default();
        let ctx = CodecCtx {
            request:       &request,
            provider_name: "bedrock",
            deployment_id: "m",
            model:         None,
            params:        &params,
        };
        assert!(encode(&ctx, false).is_err());
    }

    #[test]
    fn cache_points_follow_the_anthropic_placement() {
        let mut messages = vec![
            json!({"role": "user", "content": [{"text": "turn 1"}]}),
            json!({"role": "assistant", "content": [{"text": "reply 1"}]}),
            json!({"role": "user", "content": [{"text": "turn 2"}]}),
        ];
        apply_cache_point_to_conversation_prefix(&mut messages);
        // Second-to-last user message gains the cachePoint.
        assert!(messages[0]["content"][1].get("cachePoint").is_some());
        assert_eq!(messages[2]["content"].as_array().unwrap().len(), 1);
    }
}
