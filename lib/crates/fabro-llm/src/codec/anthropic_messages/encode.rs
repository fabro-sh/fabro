//! Request encoding: canonical request → Anthropic Messages body + headers.
//!
//! Pure and sync. File-backed attachments are resolved to inline data by
//! `attachments::resolve` in the adapter *before* encode runs, so the content
//! translation here never touches the filesystem.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use super::SYNTHETIC_TOOL_NAME;
use super::wire::{ApiMessage, ApiRequest, ApiToolDef, CacheControl, CountTokensRequest};
use crate::codec::{AnthropicVersion, CodecCtx, EncodedRequest};
use crate::providers::common;
use crate::types::{
    ContentPart, Message, ReasoningEffort, ReasoningEffortFeature, Request, ResponseFormatType,
    Role, Speed, ThinkingData, ToolChoice, ToolDefinition,
};

const CACHE_BETA_HEADER: &str = "prompt-caching-2024-07-31";
const FAST_MODE_BETA_HEADER: &str = "fast-mode-2026-02-01";
const CONTEXT_1M_BETA_HEADER: &str = "context-1m-2025-08-07";

/// Known `provider_options.anthropic` keys handled directly by the codec; not
/// re-merged into the body.
const KNOWN_ANTHROPIC_OPTION_KEYS: &[&str] = &["thinking", "auto_cache", "beta_headers"];

// --- Public entry points -----------------------------------------------------

pub(super) fn encode(ctx: &CodecCtx<'_>, stream: bool) -> EncodedRequest {
    let built = build_request(ctx, stream);
    let body = merge_provider_options(&built.request, ctx.request.provider_options.as_ref());
    EncodedRequest {
        body,
        endpoint: "/messages".to_string(),
        headers: build_headers(ctx, &built),
    }
}

pub(super) fn encode_count_tokens(ctx: &CodecCtx<'_>) -> EncodedRequest {
    let built = build_request(ctx, false);
    let headers = build_headers(ctx, &built);
    let count_request = CountTokensRequest::from(built.request);
    let body = serde_json::to_value(&count_request).unwrap_or_else(|_| serde_json::json!({}));
    EncodedRequest {
        body,
        endpoint: "/messages/count_tokens".to_string(),
        headers,
    }
}

/// The assembled `ApiRequest` plus the inputs needed to build dialect headers.
struct Built {
    request:            ApiRequest,
    auto_cache:         bool,
    is_fast:            bool,
    include_1m_context: bool,
}

fn build_headers(ctx: &CodecCtx<'_>, built: &Built) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    if let AnthropicVersion::Header(version) = ctx.params.anthropic_version {
        headers.push(("anthropic-version".to_string(), version.to_string()));
    }
    if ctx.params.anthropic_beta {
        if let Some(beta) = build_beta_header(
            ctx.request.provider_options.as_ref(),
            built.auto_cache,
            built.is_fast,
            built.include_1m_context,
        ) {
            headers.push(("anthropic-beta".to_string(), beta));
        }
    }
    headers
}

fn build_request(ctx: &CodecCtx<'_>, stream: bool) -> Built {
    let request = ctx.request;
    let (system, other_messages) = common::extract_system_prompt(&request.messages);
    let mut api_messages = translate_messages(&other_messages);

    let mut omit_tools = false;
    let tool_choice_json = request.tool_choice.as_ref().and_then(|tc| {
        if matches!(tc, ToolChoice::None) {
            omit_tools = true;
            None
        } else {
            translate_tool_choice(tc)
        }
    });

    let mut api_tools = if omit_tools {
        None
    } else {
        request.tools.as_ref().map(|t| translate_tools(t))
    };

    let model_info = ctx.model;
    let supports_prompt_cache = model_info.is_some_and(|m| m.features.prompt_cache);
    let auto_cache =
        supports_prompt_cache && is_auto_cache_enabled(request.provider_options.as_ref());

    let mut system_value = system.and_then(|s| {
        if s.trim().is_empty() {
            None
        } else if auto_cache {
            Some(system_with_cache_control(&s))
        } else {
            Some(serde_json::Value::String(s))
        }
    });

    // Apply response_format (may inject synthetic tool or system prompt suffix).
    let mut tool_choice_json = tool_choice_json;
    apply_response_format(
        request,
        &mut api_tools,
        &mut tool_choice_json,
        &mut system_value,
    );

    if auto_cache {
        if let Some(ref mut tools) = api_tools {
            apply_cache_control_to_last_tool(tools);
        }
        apply_cache_control_to_conversation_prefix(&mut api_messages);
    }

    let explicit_thinking = extract_thinking_config(request.provider_options.as_ref());

    // Older reasoning models (e.g. claude-sonnet-4-5) need `thinking` with
    // `budget_tokens` instead of `output_config.effort`.
    let supports_effort =
        model_info.is_none_or(|m| m.features.reasoning_effort == ReasoningEffortFeature::Levels);

    let mut resolved_max_tokens = request
        .max_tokens
        .or_else(|| model_info.and_then(|m| m.limits.max_output))
        .unwrap_or(65536);

    let (mut thinking, mut output_config) = if let Some(effort) = &request.reasoning_effort {
        if supports_effort {
            (
                explicit_thinking,
                Some(serde_json::json!({"effort": <&'static str>::from(*effort)})),
            )
        } else if explicit_thinking.is_none() {
            let budget = effort_to_budget_tokens(*effort, resolved_max_tokens);
            if resolved_max_tokens <= budget {
                resolved_max_tokens = budget + 1024;
            }
            (
                Some(serde_json::json!({"type": "enabled", "budget_tokens": budget})),
                None,
            )
        } else {
            (explicit_thinking, None)
        }
    } else {
        let thinking = explicit_thinking.or_else(|| {
            if model_info
                .is_some_and(|m| m.features.reasoning_effort == ReasoningEffortFeature::Levels)
            {
                Some(serde_json::json!({"type": "adaptive"}))
            } else {
                None
            }
        });
        (thinking, None)
    };

    if tool_choice_forces_tool_use(tool_choice_json.as_ref()) {
        thinking = None;
        output_config = None;
    }

    let is_fast = request.speed == Some(Speed::Fast);
    let include_1m_context = model_info.is_some_and(|m| m.context_window() >= 1_000_000);

    let request_struct = ApiRequest {
        model: ctx.deployment_id.to_string(),
        messages: api_messages,
        max_tokens: resolved_max_tokens,
        system: system_value,
        temperature: request.temperature,
        top_p: request.top_p,
        stop_sequences: Some(request.stop_sequences.clone().unwrap_or_default()),
        tools: api_tools,
        tool_choice: tool_choice_json,
        thinking,
        output_config,
        speed: request
            .speed
            .filter(|speed| *speed != Speed::Standard)
            .map(<&'static str>::from)
            .map(str::to_string),
        metadata: request.metadata.clone(),
        stream,
    };

    Built {
        request: request_struct,
        auto_cache,
        is_fast,
        include_1m_context,
    }
}

// --- Content / message / tool translation ------------------------------------

/// Translate a unified `ContentPart` to an Anthropic content block. Sync:
/// file-backed attachments are already resolved to inline data upstream.
fn content_part_to_api(part: &ContentPart) -> Option<serde_json::Value> {
    match part {
        ContentPart::Text(text) => Some(serde_json::json!({"type": "text", "text": text})),
        ContentPart::ToolCall(tc) => Some(serde_json::json!({
            "type": "tool_use",
            "id": tc.id,
            "name": tc.name,
            "input": tc.arguments,
        })),
        ContentPart::ToolResult(tr) => {
            let content = tr
                .content
                .as_str()
                .map_or_else(|| tr.content.to_string(), str::to_string);
            Some(serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tr.tool_call_id,
                "content": content,
                "is_error": tr.is_error,
            }))
        }
        ContentPart::Thinking(td) if td.redacted => Some(serde_json::json!({
            "type": "redacted_thinking",
            "data": td.text,
        })),
        ContentPart::Thinking(ThinkingData {
            text, signature, ..
        }) => {
            let mut block = serde_json::json!({ "type": "thinking", "thinking": text });
            if let Some(sig) = signature {
                block["signature"] = serde_json::Value::String(sig.clone());
            }
            Some(block)
        }
        ContentPart::Image(img) => {
            if let Some(url) = &img.url {
                Some(serde_json::json!({"type": "image", "source": {"type": "url", "url": url}}))
            } else {
                img.data.as_ref().map(|data| {
                    let mime = img.media_type.as_deref().unwrap_or("image/png");
                    let b64 = BASE64_STANDARD.encode(data);
                    serde_json::json!({"type": "image", "source": {"type": "base64", "media_type": mime, "data": b64}})
                })
            }
        }
        ContentPart::Document(doc) => {
            if let Some(url) = &doc.url {
                Some(serde_json::json!({"type": "document", "source": {"type": "url", "url": url}}))
            } else {
                doc.data.as_ref().map(|data| {
                    let mime = doc.media_type.as_deref().unwrap_or("application/pdf");
                    let b64 = BASE64_STANDARD.encode(data);
                    serde_json::json!({"type": "document", "source": {"type": "base64", "media_type": mime, "data": b64}})
                })
            }
        }
        ContentPart::Audio(_) => Some(
            serde_json::json!({"type": "text", "text": "[Audio content not supported by this provider]"}),
        ),
        ContentPart::Other { .. } => None,
    }
}

/// Convert unified messages to Anthropic API messages (role mapping, strict
/// alternation, tool results folded into user turns).
fn translate_messages(messages: &[&Message]) -> Vec<ApiMessage> {
    let mut api_messages: Vec<ApiMessage> = Vec::new();

    for msg in messages {
        let role = match msg.role {
            Role::Assistant => "assistant",
            Role::User | Role::Tool => "user",
            Role::System | Role::Developer => continue,
        };

        let mut content = Vec::new();
        for part in &msg.content {
            if let Some(block) = content_part_to_api(part) {
                content.push(block);
            }
        }

        if content.is_empty() {
            continue;
        }

        if let Some(last) = api_messages.last_mut() {
            if last.role == role {
                last.content.extend(content);
                continue;
            }
        }

        api_messages.push(ApiMessage {
            role: role.to_string(),
            content,
        });
    }

    api_messages
}

fn translate_tools(tools: &[ToolDefinition]) -> Vec<ApiToolDef> {
    tools
        .iter()
        .map(|t| ApiToolDef {
            name:          t.name.clone(),
            description:   t.description.clone(),
            input_schema:  t.parameters.clone(),
            cache_control: None,
        })
        .collect()
}

fn translate_tool_choice(choice: &ToolChoice) -> Option<serde_json::Value> {
    match choice {
        ToolChoice::Auto => Some(serde_json::json!({"type": "auto"})),
        // Anthropic does not support tool_choice none with tools present; the
        // caller omits tools instead.
        ToolChoice::None => None,
        ToolChoice::Required => Some(serde_json::json!({"type": "any"})),
        ToolChoice::Named { tool_name } => {
            Some(serde_json::json!({"type": "tool", "name": tool_name}))
        }
    }
}

fn tool_choice_forces_tool_use(tool_choice: Option<&serde_json::Value>) -> bool {
    matches!(
        tool_choice
            .and_then(|value| value.get("type"))
            .and_then(serde_json::Value::as_str),
        Some("any" | "tool")
    )
}

// --- Structured output (response_format) -------------------------------------

fn apply_response_format(
    request: &Request,
    api_tools: &mut Option<Vec<ApiToolDef>>,
    tool_choice: &mut Option<serde_json::Value>,
    system: &mut Option<serde_json::Value>,
) {
    let Some(format) = &request.response_format else {
        return;
    };

    match format.kind {
        ResponseFormatType::JsonSchema => {
            let schema = format
                .json_schema
                .clone()
                .unwrap_or_else(|| serde_json::json!({"type": "object"}));
            let synthetic_tool = ApiToolDef {
                name:          SYNTHETIC_TOOL_NAME.to_string(),
                description:   "Output the requested structured data".to_string(),
                input_schema:  schema,
                cache_control: None,
            };
            match api_tools {
                Some(tools) => tools.push(synthetic_tool),
                None => *api_tools = Some(vec![synthetic_tool]),
            }
            *tool_choice = Some(serde_json::json!({"type": "tool", "name": SYNTHETIC_TOOL_NAME}));
        }
        ResponseFormatType::JsonObject => {
            let json_instruction = "\n\nYou must respond with valid JSON only, no other text.";
            match system {
                Some(serde_json::Value::Array(blocks)) => {
                    if let Some(last) = blocks.last_mut() {
                        if let Some(text) = last.get("text").and_then(serde_json::Value::as_str) {
                            let mut new_text = text.to_string();
                            new_text.push_str(json_instruction);
                            last["text"] = serde_json::Value::String(new_text);
                        }
                    } else {
                        blocks.push(
                            serde_json::json!({"type": "text", "text": json_instruction.trim()}),
                        );
                    }
                }
                Some(serde_json::Value::String(s)) => {
                    s.push_str(json_instruction);
                }
                None => {
                    *system = Some(serde_json::Value::String(
                        json_instruction.trim().to_string(),
                    ));
                }
                _ => {}
            }
        }
        ResponseFormatType::Text => {}
    }
}

// --- Prompt caching / thinking / beta headers --------------------------------

fn extract_thinking_config(
    provider_options: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    provider_options
        .and_then(|opts| opts.get("anthropic"))
        .and_then(|anthropic| anthropic.get("thinking"))
        .cloned()
}

fn effort_to_budget_tokens(effort: ReasoningEffort, max_tokens: i64) -> i64 {
    let budget = match effort {
        ReasoningEffort::Low => max_tokens / 4,
        ReasoningEffort::Medium => max_tokens / 2,
        ReasoningEffort::High => max_tokens * 3 / 4,
        ReasoningEffort::XHigh => max_tokens * 7 / 8,
        ReasoningEffort::Max => max_tokens,
    };
    budget.max(1024)
}

fn is_auto_cache_enabled(provider_options: Option<&serde_json::Value>) -> bool {
    provider_options
        .and_then(|opts| opts.get("anthropic"))
        .and_then(|anthropic| anthropic.get("auto_cache"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
}

fn system_with_cache_control(system: &str) -> serde_json::Value {
    serde_json::json!([{
        "type": "text",
        "text": system,
        "cache_control": {"type": "ephemeral"}
    }])
}

fn apply_cache_control_to_last_tool(tools: &mut [ApiToolDef]) {
    if let Some(last) = tools.last_mut() {
        last.cache_control = Some(CacheControl::ephemeral());
    }
}

fn apply_cache_control_to_conversation_prefix(messages: &mut [ApiMessage]) {
    let user_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "user")
        .map(|(i, _)| i)
        .collect();

    if user_indices.len() < 2 {
        return;
    }

    let target_idx = user_indices[user_indices.len() - 2];
    if let Some(serde_json::Value::Object(map)) = messages[target_idx].content.last_mut() {
        map.insert(
            "cache_control".to_string(),
            serde_json::json!({"type": "ephemeral"}),
        );
    }
}

fn build_beta_header(
    provider_options: Option<&serde_json::Value>,
    include_cache_header: bool,
    include_fast_mode_header: bool,
    include_1m_context: bool,
) -> Option<String> {
    let mut headers: Vec<String> = Vec::new();

    if let Some(beta_array) = provider_options
        .and_then(|opts| opts.get("anthropic"))
        .and_then(|anthropic| anthropic.get("beta_headers"))
        .and_then(serde_json::Value::as_array)
    {
        headers.extend(
            beta_array
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(String::from),
        );
    }

    if include_cache_header && !headers.iter().any(|h| h == CACHE_BETA_HEADER) {
        headers.push(CACHE_BETA_HEADER.to_string());
    }

    if include_fast_mode_header && !headers.iter().any(|h| h == FAST_MODE_BETA_HEADER) {
        headers.push(FAST_MODE_BETA_HEADER.to_string());
    }

    if include_1m_context && !headers.iter().any(|h| h == CONTEXT_1M_BETA_HEADER) {
        headers.push(CONTEXT_1M_BETA_HEADER.to_string());
    }

    if headers.is_empty() {
        None
    } else {
        Some(headers.join(","))
    }
}

/// Serialize the API request and merge any unknown `provider_options.anthropic`
/// keys into the body.
fn merge_provider_options(
    api_request: &ApiRequest,
    provider_options: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut body = serde_json::to_value(api_request).unwrap_or_else(|_| serde_json::json!({}));

    if let Some(anthropic_opts) = provider_options.and_then(|opts| opts.get("anthropic")) {
        if let (Some(base), Some(overrides)) = (body.as_object_mut(), anthropic_opts.as_object()) {
            for (key, value) in overrides {
                if !KNOWN_ANTHROPIC_OPTION_KEYS.contains(&key.as_str()) {
                    base.insert(key.clone(), value.clone());
                }
            }
        }
    }

    body
}
