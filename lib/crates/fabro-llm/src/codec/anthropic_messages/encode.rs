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

/// Known `provider_options.anthropic` keys handled directly by the codec; not
/// re-merged into the body.
const KNOWN_ANTHROPIC_OPTION_KEYS: &[&str] = &["thinking", "auto_cache", "beta_headers"];

// --- Public entry points -----------------------------------------------------

pub(super) fn encode(ctx: &CodecCtx<'_>, stream: bool) -> EncodedRequest {
    let request = build_request(ctx, stream);
    let body = merge_provider_options(&request, ctx.request.provider_options.as_ref());
    EncodedRequest {
        body,
        endpoint: "/messages".to_string(),
        headers: build_headers(ctx),
    }
}

pub(super) fn encode_count_tokens(ctx: &CodecCtx<'_>) -> EncodedRequest {
    let count_request = CountTokensRequest::from(build_request(ctx, false));
    let body = serde_json::to_value(&count_request).unwrap_or_else(|_| serde_json::json!({}));
    EncodedRequest {
        body,
        endpoint: "/messages/count_tokens".to_string(),
        headers: build_headers(ctx),
    }
}

/// Whether auto prompt-caching applies: the model supports it and the request
/// hasn't opted out.
fn auto_cache(ctx: &CodecCtx<'_>) -> bool {
    ctx.model.is_some_and(|m| m.features.prompt_cache)
        && is_auto_cache_enabled(ctx.request.provider_options.as_ref())
}

fn build_headers(ctx: &CodecCtx<'_>) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    if let AnthropicVersion::Header(version) = ctx.params.anthropic_version {
        headers.push(("anthropic-version".to_string(), version.to_string()));
    }
    if ctx.params.anthropic_beta {
        if let Some(beta) = build_beta_header(
            ctx.request.provider_options.as_ref(),
            auto_cache(ctx),
            ctx.request.speed == Some(Speed::Fast),
        ) {
            headers.push(("anthropic-beta".to_string(), beta));
        }
    }
    headers
}

fn build_request(ctx: &CodecCtx<'_>, stream: bool) -> ApiRequest {
    let request = ctx.request;
    let (system, other_messages) = common::extract_system_prompt(&request.messages);
    let mut api_messages = translate_messages(&other_messages);

    // `ToolChoice::None` omits the tools entirely instead of sending a choice.
    let omit_tools = matches!(request.tool_choice, Some(ToolChoice::None));
    let mut tool_choice_json = if omit_tools {
        None
    } else {
        request.tool_choice.as_ref().and_then(translate_tool_choice)
    };

    let mut api_tools = if omit_tools {
        None
    } else {
        request.tools.as_ref().map(|t| translate_tools(t))
    };

    let model_info = ctx.model;
    let auto_cache = auto_cache(ctx);

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
    let supports_effort = model_info.is_none_or(fabro_model::Model::supports_reasoning_effort);

    let mut resolved_max_tokens = request
        .max_tokens
        .or_else(|| model_info.and_then(|m| m.limits.max_output))
        .unwrap_or(65536);

    // Default thinking when none is configured explicitly: adaptive for
    // `levels` models, with or without an effort level — effort is guidance
    // for thinking allocation, not a replacement for it. Natively adaptive
    // models don't need one injected (and reject a manual on/off toggle).
    let default_thinking = || {
        if model_info.is_some_and(|m| m.features.reasoning_effort == ReasoningEffortFeature::Levels)
        {
            Some(serde_json::json!({"type": "adaptive"}))
        } else {
            None
        }
    };

    let (mut thinking, mut output_config) = if let Some(effort) = &request.reasoning_effort {
        if supports_effort {
            (
                explicit_thinking.or_else(default_thinking),
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
        (explicit_thinking.or_else(default_thinking), None)
    };

    if tool_choice_forces_tool_use(tool_choice_json.as_ref()) {
        thinking = None;
        output_config = None;
    }

    // Models with `sampling_params = false` reject classic sampling knobs.
    // This gate covers only the typed request fields; values injected through
    // `provider_options.anthropic` (e.g. `top_k`) are a raw escape hatch and
    // pass through unfiltered.
    let (temperature, top_p) =
        if model_info.is_none_or(fabro_model::Model::supports_sampling_params) {
            (request.temperature, request.top_p)
        } else {
            (None, None)
        };

    ApiRequest {
        model: ctx.deployment_id.to_string(),
        messages: api_messages,
        max_tokens: resolved_max_tokens,
        system: system_value,
        temperature,
        top_p,
        stop_sequences: request.stop_sequences.clone().unwrap_or_default(),
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
        ContentPart::Image(img) => media_block(
            "image",
            img.url.as_deref(),
            img.data.as_deref(),
            img.media_type.as_deref().unwrap_or("image/png"),
        ),
        ContentPart::Document(doc) => media_block(
            "document",
            doc.url.as_deref(),
            doc.data.as_deref(),
            doc.media_type.as_deref().unwrap_or("application/pdf"),
        ),
        ContentPart::Audio(_) => Some(
            serde_json::json!({"type": "text", "text": "[Audio content not supported by this provider]"}),
        ),
        ContentPart::Other { .. } => None,
    }
}

/// An `image`/`document` content block: URL source when present, otherwise
/// base64-encoded inline data.
fn media_block(
    kind: &str,
    url: Option<&str>,
    data: Option<&[u8]>,
    mime: &str,
) -> Option<serde_json::Value> {
    if let Some(url) = url {
        Some(serde_json::json!({"type": kind, "source": {"type": "url", "url": url}}))
    } else {
        data.map(|data| {
            let b64 = BASE64_STANDARD.encode(data);
            serde_json::json!({"type": kind, "source": {"type": "base64", "media_type": mime, "data": b64}})
        })
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

/// The `provider_options.anthropic` namespace object, if any.
fn anthropic_options(provider_options: Option<&serde_json::Value>) -> Option<&serde_json::Value> {
    provider_options.and_then(|opts| opts.get("anthropic"))
}

/// A single `provider_options.anthropic.<key>` value, if any. `pub(crate)` so
/// the adapter's `validate_request` reads the same namespace the same way.
pub(crate) fn anthropic_option<'a>(
    provider_options: Option<&'a serde_json::Value>,
    key: &str,
) -> Option<&'a serde_json::Value> {
    anthropic_options(provider_options).and_then(|anthropic| anthropic.get(key))
}

fn extract_thinking_config(
    provider_options: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    anthropic_option(provider_options, "thinking").cloned()
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
    anthropic_option(provider_options, "auto_cache")
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
) -> Option<String> {
    let mut headers: Vec<String> = Vec::new();

    if let Some(beta_array) =
        anthropic_option(provider_options, "beta_headers").and_then(serde_json::Value::as_array)
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

    if let Some(anthropic_opts) = anthropic_options(provider_options) {
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

#[cfg(test)]
mod tests {
    use fabro_model::Catalog;
    use fabro_model::catalog::LlmCatalogSettings;

    use super::*;
    use crate::codec::CodecParams;
    use crate::types::{AudioData, DocumentData, ResponseFormat};

    // --- Test helpers --------------------------------------------------------

    fn make_base_request() -> Request {
        Request {
            model:            "claude-sonnet-4-20250514".to_string(),
            messages:         vec![Message::user("Hello")],
            provider:         Some("anthropic".to_string()),
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

    fn make_request_with_format(format: ResponseFormat) -> Request {
        Request {
            provider: None,
            response_format: Some(format),
            max_tokens: None,
            ..make_base_request()
        }
    }

    fn catalog_with_anthropic_model(features: &str) -> Catalog {
        let settings: LlmCatalogSettings = toml::from_str(&format!(
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
{features}
"#
        ))
        .unwrap();
        Catalog::from_settings(&settings).unwrap()
    }

    /// Direct-Anthropic route params (version header + beta headers enabled),
    /// matching what the adapter's `route_config()` resolves for "anthropic".
    fn direct_params() -> CodecParams {
        CodecParams {
            anthropic_version: AnthropicVersion::Header("2023-06-01"),
            anthropic_beta:    true,
        }
    }

    /// Encode `request` on the direct-Anthropic route, optionally with a
    /// catalog (for capability-driven behavior like prompt-cache/effort).
    fn encode_direct(request: &Request, catalog: Option<&Catalog>, stream: bool) -> EncodedRequest {
        let deployment_id = common::api_model_id(catalog, &request.model);
        let params = direct_params();
        let ctx = CodecCtx {
            request,
            provider_name: "anthropic",
            deployment_id: &deployment_id,
            model: common::catalog_model(catalog, &request.model),
            params: &params,
        };
        encode(&ctx, stream)
    }

    fn encode_count_direct(request: &Request, catalog: Option<&Catalog>) -> EncodedRequest {
        let deployment_id = common::api_model_id(catalog, &request.model);
        let params = direct_params();
        let ctx = CodecCtx {
            request,
            provider_name: "anthropic",
            deployment_id: &deployment_id,
            model: common::catalog_model(catalog, &request.model),
            params: &params,
        };
        encode_count_tokens(&ctx)
    }

    fn header_value<'a>(encoded: &'a EncodedRequest, name: &str) -> Option<&'a str> {
        encoded
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    // --- auto_cache ----------------------------------------------------------

    #[test]
    fn auto_cache_enabled_by_default() {
        assert!(is_auto_cache_enabled(None));
    }

    #[test]
    fn auto_cache_enabled_when_true() {
        let opts = serde_json::json!({"anthropic": {"auto_cache": true}});
        assert!(is_auto_cache_enabled(Some(&opts)));
    }

    #[test]
    fn auto_cache_disabled_when_false() {
        let opts = serde_json::json!({"anthropic": {"auto_cache": false}});
        assert!(!is_auto_cache_enabled(Some(&opts)));
    }

    #[test]
    fn auto_cache_enabled_when_key_missing() {
        let opts = serde_json::json!({"anthropic": {}});
        assert!(is_auto_cache_enabled(Some(&opts)));
    }

    #[test]
    fn auto_cache_enabled_when_anthropic_missing() {
        let opts = serde_json::json!({"openai": {}});
        assert!(is_auto_cache_enabled(Some(&opts)));
    }

    // --- prompt-cache helpers ------------------------------------------------

    #[test]
    fn system_prompt_cache_control_wraps_as_array() {
        let result = system_with_cache_control("You are helpful.");
        let arr = result.as_array().expect("should be an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "You are helpful.");
        assert_eq!(arr[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn tool_cache_control_applied_to_last_tool() {
        let mut tools = vec![
            ApiToolDef {
                name:          "tool_a".to_string(),
                description:   "first".to_string(),
                input_schema:  serde_json::json!({}),
                cache_control: None,
            },
            ApiToolDef {
                name:          "tool_b".to_string(),
                description:   "second".to_string(),
                input_schema:  serde_json::json!({}),
                cache_control: None,
            },
        ];
        apply_cache_control_to_last_tool(&mut tools);

        assert!(tools[0].cache_control.is_none());
        assert!(tools[1].cache_control.is_some());
        assert_eq!(tools[1].cache_control.as_ref().unwrap().kind, "ephemeral");
    }

    #[test]
    fn tool_cache_control_empty_slice() {
        let mut tools: Vec<ApiToolDef> = vec![];
        apply_cache_control_to_last_tool(&mut tools);
        assert!(tools.is_empty());
    }

    #[test]
    fn tool_cache_control_single_tool() {
        let mut tools = vec![ApiToolDef {
            name:          "only_tool".to_string(),
            description:   "the one".to_string(),
            input_schema:  serde_json::json!({}),
            cache_control: None,
        }];
        apply_cache_control_to_last_tool(&mut tools);
        assert!(tools[0].cache_control.is_some());
    }

    #[test]
    fn conversation_prefix_cache_control_with_two_user_messages() {
        let mut messages = vec![
            ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Hello"})],
            },
            ApiMessage {
                role:    "assistant".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Hi there"})],
            },
            ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "How are you?"})],
            },
        ];

        apply_cache_control_to_conversation_prefix(&mut messages);

        // First user message should have cache_control
        assert_eq!(messages[0].content[0]["cache_control"]["type"], "ephemeral");
        // Last user message should NOT have cache_control
        assert!(messages[2].content[0].get("cache_control").is_none());
        // Assistant message should NOT have cache_control
        assert!(messages[1].content[0].get("cache_control").is_none());
    }

    #[test]
    fn conversation_prefix_cache_control_with_multiple_content_blocks() {
        let mut messages = vec![
            ApiMessage {
                role:    "user".to_string(),
                content: vec![
                    serde_json::json!({"type": "text", "text": "Part 1"}),
                    serde_json::json!({"type": "text", "text": "Part 2"}),
                ],
            },
            ApiMessage {
                role:    "assistant".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Reply"})],
            },
            ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Follow up"})],
            },
        ];

        apply_cache_control_to_conversation_prefix(&mut messages);

        // Only the LAST content block of the first user message should have
        // cache_control
        assert!(messages[0].content[0].get("cache_control").is_none());
        assert_eq!(messages[0].content[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn conversation_prefix_cache_control_single_user_message() {
        let mut messages = vec![ApiMessage {
            role:    "user".to_string(),
            content: vec![serde_json::json!({"type": "text", "text": "Hello"})],
        }];

        apply_cache_control_to_conversation_prefix(&mut messages);

        // With only one user message, no cache_control should be added
        assert!(messages[0].content[0].get("cache_control").is_none());
    }

    #[test]
    fn conversation_prefix_cache_control_no_user_messages() {
        let mut messages: Vec<ApiMessage> = vec![];
        // Should not panic on empty messages
        apply_cache_control_to_conversation_prefix(&mut messages);
    }

    #[test]
    fn conversation_prefix_cache_control_three_user_messages() {
        let mut messages = vec![
            ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "First"})],
            },
            ApiMessage {
                role:    "assistant".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Reply 1"})],
            },
            ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Second"})],
            },
            ApiMessage {
                role:    "assistant".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Reply 2"})],
            },
            ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Third"})],
            },
        ];

        apply_cache_control_to_conversation_prefix(&mut messages);

        // Only the second-to-last user message (index 2) should get cache_control
        assert!(messages[0].content[0].get("cache_control").is_none());
        assert_eq!(messages[2].content[0]["cache_control"]["type"], "ephemeral");
        assert!(messages[4].content[0].get("cache_control").is_none());
    }

    // --- beta headers --------------------------------------------------------

    #[test]
    fn beta_header_includes_cache_header() {
        let result = build_beta_header(None, true, false);
        assert_eq!(result, Some(CACHE_BETA_HEADER.to_string()));
    }

    #[test]
    fn beta_header_no_cache_no_user_headers() {
        let result = build_beta_header(None, false, false);
        assert_eq!(result, None);
    }

    #[test]
    fn beta_header_merges_user_headers_with_cache() {
        let opts = serde_json::json!({
            "anthropic": {
                "beta_headers": ["interleaved-thinking-2025-05-14"]
            }
        });
        let result = build_beta_header(Some(&opts), true, false);
        assert_eq!(
            result,
            Some(format!(
                "interleaved-thinking-2025-05-14,{CACHE_BETA_HEADER}"
            ))
        );
    }

    #[test]
    fn beta_header_no_duplicate_cache_header() {
        let opts = serde_json::json!({
            "anthropic": {
                "beta_headers": [CACHE_BETA_HEADER]
            }
        });
        let result = build_beta_header(Some(&opts), true, false);
        // Should not duplicate the header
        assert_eq!(result, Some(CACHE_BETA_HEADER.to_string()));
    }

    #[test]
    fn beta_header_user_headers_only_when_cache_disabled() {
        let opts = serde_json::json!({
            "anthropic": {
                "beta_headers": ["interleaved-thinking-2025-05-14"]
            }
        });
        let result = build_beta_header(Some(&opts), false, false);
        assert_eq!(result, Some("interleaved-thinking-2025-05-14".to_string()));
    }

    /// Regression test: deprecated beta header values must not be sent.
    /// The Anthropic API rejects requests containing these old headers.
    #[test]
    fn beta_header_rejects_deprecated_values() {
        let deprecated = [
            "extended-thinking-2025-04-14",
            "max-tokens-3-5-sonnet-2025-04-14",
        ];

        // No user headers — only cache header should appear
        let header = build_beta_header(None, true, false).unwrap_or_default();
        for dep in &deprecated {
            assert!(
                !header.contains(dep),
                "default header must not contain deprecated value {dep}"
            );
        }

        // With a valid user header
        let opts = serde_json::json!({
            "anthropic": {
                "beta_headers": ["interleaved-thinking-2025-05-14"]
            }
        });
        let header = build_beta_header(Some(&opts), true, false).unwrap_or_default();
        for dep in &deprecated {
            assert!(
                !header.contains(dep),
                "header with user values must not contain deprecated value {dep}"
            );
        }
    }

    #[test]
    fn beta_header_includes_both_cache_and_fast_mode() {
        let result = build_beta_header(None, true, true);
        let header = result.expect("should produce a header");
        assert!(
            header.contains(CACHE_BETA_HEADER),
            "should contain cache header"
        );
        assert!(
            header.contains(FAST_MODE_BETA_HEADER),
            "should contain fast-mode header"
        );
    }

    // --- effort → thinking budget --------------------------------------------

    #[test]
    fn effort_to_budget_tokens_xhigh_maps_to_seven_eighths() {
        assert_eq!(
            effort_to_budget_tokens(ReasoningEffort::XHigh, 16_000),
            14_000
        );
    }

    #[test]
    fn effort_to_budget_tokens_max_maps_to_full_budget() {
        assert_eq!(
            effort_to_budget_tokens(ReasoningEffort::Max, 16_000),
            16_000
        );
    }

    // --- system prompt serialization -----------------------------------------

    #[test]
    fn system_prompt_as_string_when_cache_disabled() {
        let system = "You are helpful.".to_string();
        let value = serde_json::Value::String(system);
        assert_eq!(value.as_str(), Some("You are helpful."));
    }

    #[test]
    fn api_request_serialization_with_cached_system() {
        let api_request = ApiRequest {
            model:          "claude-sonnet-4-20250514".to_string(),
            messages:       vec![ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Hello"})],
            }],
            max_tokens:     4096,
            system:         Some(system_with_cache_control("You are helpful.")),
            temperature:    None,
            top_p:          None,
            stop_sequences: Vec::new(),
            tools:          None,
            tool_choice:    None,
            thinking:       None,
            output_config:  None,
            speed:          None,
            metadata:       None,
            stream:         false,
        };

        let json = serde_json::to_value(&api_request).expect("should serialize");
        let system = json.get("system").expect("system should be present");
        let arr = system.as_array().expect("system should be an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["cache_control"]["type"], "ephemeral");
    }

    // --- response_format ------------------------------------------------------

    #[test]
    fn response_format_json_schema_injects_synthetic_tool() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"]
        });
        let request = make_request_with_format(ResponseFormat {
            kind:        ResponseFormatType::JsonSchema,
            json_schema: Some(schema.clone()),
            strict:      false,
        });

        let mut tools: Option<Vec<ApiToolDef>> = None;
        let mut tool_choice: Option<serde_json::Value> = None;
        let mut system: Option<serde_json::Value> = None;

        apply_response_format(&request, &mut tools, &mut tool_choice, &mut system);

        let tools = tools.expect("tools should be set");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, SYNTHETIC_TOOL_NAME);
        assert_eq!(tools[0].input_schema, schema);

        let tc = tool_choice.expect("tool_choice should be set");
        assert_eq!(tc["type"], "tool");
        assert_eq!(tc["name"], SYNTHETIC_TOOL_NAME);

        // System should not be modified
        assert!(system.is_none());
    }

    #[test]
    fn tool_choice_forces_tool_use_detects_forced_modes() {
        assert!(tool_choice_forces_tool_use(Some(
            &serde_json::json!({"type": "any"})
        )));
        assert!(tool_choice_forces_tool_use(Some(
            &serde_json::json!({"type": "tool", "name": "json_output"})
        )));

        assert!(!tool_choice_forces_tool_use(Some(
            &serde_json::json!({"type": "auto"})
        )));
        assert!(!tool_choice_forces_tool_use(Some(
            &serde_json::json!({"type": "none"})
        )));
        assert!(!tool_choice_forces_tool_use(None));
    }

    #[test]
    fn response_format_json_schema_appends_to_existing_tools() {
        let schema = serde_json::json!({"type": "object"});
        let mut request = make_request_with_format(ResponseFormat {
            kind:        ResponseFormatType::JsonSchema,
            json_schema: Some(schema),
            strict:      false,
        });
        request.tools = Some(vec![ToolDefinition {
            name:        "existing_tool".to_string(),
            description: "An existing tool".to_string(),
            parameters:  serde_json::json!({}),
        }]);

        let mut tools: Option<Vec<ApiToolDef>> =
            Some(translate_tools(request.tools.as_ref().unwrap()));
        let mut tool_choice: Option<serde_json::Value> = None;
        let mut system: Option<serde_json::Value> = None;

        apply_response_format(&request, &mut tools, &mut tool_choice, &mut system);

        let tools = tools.expect("tools should be set");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "existing_tool");
        assert_eq!(tools[1].name, SYNTHETIC_TOOL_NAME);
    }

    #[test]
    fn response_format_json_object_appends_to_string_system() {
        let request = make_request_with_format(ResponseFormat {
            kind:        ResponseFormatType::JsonObject,
            json_schema: None,
            strict:      false,
        });

        let mut tools: Option<Vec<ApiToolDef>> = None;
        let mut tool_choice: Option<serde_json::Value> = None;
        let mut system = Some(serde_json::Value::String("You are helpful.".to_string()));

        apply_response_format(&request, &mut tools, &mut tool_choice, &mut system);

        let sys = system.expect("system should be set");
        let text = sys.as_str().expect("should be a string");
        assert!(text.contains("You are helpful."));
        assert!(text.contains("valid JSON"));

        // Tools should not be modified
        assert!(tools.is_none());
        assert!(tool_choice.is_none());
    }

    #[test]
    fn response_format_json_object_sets_system_when_none() {
        let request = make_request_with_format(ResponseFormat {
            kind:        ResponseFormatType::JsonObject,
            json_schema: None,
            strict:      false,
        });

        let mut tools: Option<Vec<ApiToolDef>> = None;
        let mut tool_choice: Option<serde_json::Value> = None;
        let mut system: Option<serde_json::Value> = None;

        apply_response_format(&request, &mut tools, &mut tool_choice, &mut system);

        let sys = system.expect("system should be set");
        let text = sys.as_str().expect("should be a string");
        assert!(text.contains("valid JSON"));
    }

    #[test]
    fn response_format_json_object_appends_to_array_system() {
        let request = make_request_with_format(ResponseFormat {
            kind:        ResponseFormatType::JsonObject,
            json_schema: None,
            strict:      false,
        });

        let mut tools: Option<Vec<ApiToolDef>> = None;
        let mut tool_choice: Option<serde_json::Value> = None;
        let mut system = Some(system_with_cache_control("You are helpful."));

        apply_response_format(&request, &mut tools, &mut tool_choice, &mut system);

        let sys = system.expect("system should be set");
        let arr = sys.as_array().expect("should be an array");
        let text = arr[0]["text"].as_str().expect("should have text");
        assert!(text.contains("You are helpful."));
        assert!(text.contains("valid JSON"));
    }

    #[test]
    fn response_format_text_is_noop() {
        let request = make_request_with_format(ResponseFormat {
            kind:        ResponseFormatType::Text,
            json_schema: None,
            strict:      false,
        });

        let mut tools: Option<Vec<ApiToolDef>> = None;
        let mut tool_choice: Option<serde_json::Value> = None;
        let mut system: Option<serde_json::Value> = None;

        apply_response_format(&request, &mut tools, &mut tool_choice, &mut system);

        assert!(tools.is_none());
        assert!(tool_choice.is_none());
        assert!(system.is_none());
    }

    // --- merge_provider_options ----------------------------------------------

    #[test]
    fn merge_provider_options_passes_through_unknown_keys() {
        let api_request = ApiRequest {
            model:          "claude-sonnet-4-20250514".to_string(),
            messages:       vec![ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Hello"})],
            }],
            max_tokens:     4096,
            system:         None,
            temperature:    None,
            top_p:          None,
            stop_sequences: Vec::new(),
            tools:          None,
            tool_choice:    None,
            thinking:       None,
            output_config:  None,
            speed:          None,
            metadata:       None,
            stream:         false,
        };

        let opts = serde_json::json!({
            "anthropic": {
                "top_k": 40,
                "custom_field": "value"
            }
        });
        let body = merge_provider_options(&api_request, Some(&opts));
        assert_eq!(body["top_k"], 40);
        assert_eq!(body["custom_field"], "value");
    }

    #[test]
    fn merge_provider_options_skips_known_keys() {
        let api_request = ApiRequest {
            model:          "claude-sonnet-4-20250514".to_string(),
            messages:       vec![ApiMessage {
                role:    "user".to_string(),
                content: vec![serde_json::json!({"type": "text", "text": "Hello"})],
            }],
            max_tokens:     4096,
            system:         None,
            temperature:    None,
            top_p:          None,
            stop_sequences: Vec::new(),
            tools:          None,
            tool_choice:    None,
            thinking:       None,
            output_config:  None,
            speed:          None,
            metadata:       None,
            stream:         false,
        };

        let opts = serde_json::json!({
            "anthropic": {
                "thinking": {"type": "enabled", "budget_tokens": 10000},
                "auto_cache": false,
                "beta_headers": ["some-header"],
                "top_k": 40
            }
        });
        let body = merge_provider_options(&api_request, Some(&opts));
        // Known keys should not be merged (they are handled separately)
        assert!(body.get("auto_cache").is_none());
        assert!(body.get("beta_headers").is_none());
        // thinking is handled by the ApiRequest struct directly, should not be
        // double-merged
        assert!(body["thinking"].is_null());
        // Unknown keys should be merged
        assert_eq!(body["top_k"], 40);
    }

    // --- content_part_to_api (documents / audio) -----------------------------

    #[test]
    fn document_url_translates_to_url_source() {
        let part = ContentPart::Document(DocumentData {
            url:        Some("https://example.com/doc.pdf".to_string()),
            data:       None,
            media_type: None,
            file_name:  None,
        });
        let result = content_part_to_api(&part).expect("should produce JSON");
        assert_eq!(result["type"], "document");
        assert_eq!(result["source"]["type"], "url");
        assert_eq!(result["source"]["url"], "https://example.com/doc.pdf");
    }

    #[test]
    fn document_base64_data_translates_to_base64_source() {
        let part = ContentPart::Document(DocumentData {
            url:        None,
            data:       Some(vec![0x25, 0x50, 0x44, 0x46]),
            media_type: Some("application/pdf".to_string()),
            file_name:  Some("test.pdf".to_string()),
        });
        let result = content_part_to_api(&part).expect("should produce JSON");
        assert_eq!(result["type"], "document");
        assert_eq!(result["source"]["type"], "base64");
        assert_eq!(result["source"]["media_type"], "application/pdf");
        assert!(result["source"]["data"].as_str().is_some());
    }

    #[test]
    fn document_base64_defaults_to_pdf_mime() {
        let part = ContentPart::Document(DocumentData {
            url:        None,
            data:       Some(vec![1, 2, 3]),
            media_type: None,
            file_name:  None,
        });
        let result = content_part_to_api(&part).expect("should produce JSON");
        assert_eq!(result["source"]["media_type"], "application/pdf");
    }

    #[test]
    fn audio_produces_text_fallback() {
        let part = ContentPart::Audio(AudioData {
            url:        Some("https://example.com/audio.wav".to_string()),
            data:       None,
            media_type: None,
        });
        let result = content_part_to_api(&part).expect("should produce JSON");
        assert_eq!(result["type"], "text");
        assert_eq!(
            result["text"],
            "[Audio content not supported by this provider]"
        );
    }

    // --- end-to-end encode (formerly build_api_request) ----------------------

    #[test]
    fn build_request_omits_whitespace_only_system_prompt() {
        let request = Request {
            messages: vec![Message::system("   \n\t"), Message::user("Hello")],
            ..make_base_request()
        };

        let encoded = encode_direct(&request, None, false);
        assert!(
            encoded.body.get("system").is_none(),
            "whitespace-only system prompts should be omitted"
        );
    }

    #[test]
    fn build_request_maps_reasoning_effort_to_output_config() {
        let request = Request {
            reasoning_effort: Some(ReasoningEffort::Medium),
            ..make_base_request()
        };

        let encoded = encode_direct(&request, None, false);
        assert_eq!(
            encoded.body["output_config"],
            serde_json::json!({"effort": "medium"})
        );
    }

    #[test]
    fn build_request_disables_prompt_cache_when_model_feature_is_false() {
        let catalog = catalog_with_anthropic_model(
            r#"
reasoning_effort = "levels"
prompt_cache = false
"#,
        );
        let request = Request {
            model: "test-claude".to_string(),
            messages: vec![
                Message::system("Use the cache if supported."),
                Message::user("Hello"),
            ],
            provider_options: Some(serde_json::json!({
                "anthropic": {"auto_cache": true}
            })),
            ..make_base_request()
        };

        let encoded = encode_direct(&request, Some(&catalog), false);
        assert_eq!(
            encoded.body["system"],
            serde_json::json!("Use the cache if supported.")
        );
        let beta = header_value(&encoded, "anthropic-beta");
        assert!(
            beta.is_none_or(|value| !value.contains(CACHE_BETA_HEADER)),
            "cache beta header must not be sent when the model disables prompt cache"
        );
    }

    #[test]
    fn build_request_without_injected_catalog_does_not_use_builtin_model_metadata() {
        let request = Request {
            model: "claude-sonnet-4-5".to_string(),
            messages: vec![
                Message::system("Do not infer cache support from built-ins."),
                Message::user("Hello"),
            ],
            provider_options: Some(serde_json::json!({
                "anthropic": {"auto_cache": true}
            })),
            ..make_base_request()
        };

        let encoded = encode_direct(&request, None, false);
        assert_eq!(
            encoded.body["system"],
            serde_json::json!("Do not infer cache support from built-ins.")
        );
        let beta = header_value(&encoded, "anthropic-beta");
        assert!(
            beta.is_none_or(|value| !value.contains(CACHE_BETA_HEADER)),
            "cache beta header must require injected model metadata"
        );
    }

    #[test]
    fn build_request_enables_prompt_cache_when_model_feature_is_true() {
        let catalog = catalog_with_anthropic_model(
            r#"
reasoning_effort = "levels"
prompt_cache = true
"#,
        );
        let request = Request {
            model: "test-claude".to_string(),
            messages: vec![
                Message::system("Use the cache if supported."),
                Message::user("Hello"),
            ],
            ..make_base_request()
        };

        let encoded = encode_direct(&request, Some(&catalog), false);
        assert_eq!(
            encoded.body["system"][0]["cache_control"]["type"],
            "ephemeral"
        );
        let beta =
            header_value(&encoded, "anthropic-beta").expect("cache beta header should be present");
        assert!(beta.contains(CACHE_BETA_HEADER));
    }

    #[test]
    fn build_request_uses_adaptive_thinking_for_injected_effort_model_without_forced_tools() {
        let catalog = catalog_with_anthropic_model(
            r#"
reasoning_effort = "levels"
"#,
        );
        let request = Request {
            model: "test-claude".to_string(),
            ..make_base_request()
        };

        let encoded = encode_direct(&request, Some(&catalog), false);
        assert_eq!(
            encoded.body["thinking"],
            serde_json::json!({"type": "adaptive"})
        );
    }

    #[test]
    fn build_request_omits_thinking_for_opus_4_7_json_schema() {
        let request = Request {
            model: "claude-opus-4-7".to_string(),
            response_format: Some(ResponseFormat {
                kind:        ResponseFormatType::JsonSchema,
                json_schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"title": {"type": "string"}},
                    "required": ["title"]
                })),
                strict:      true,
            }),
            ..make_base_request()
        };

        let encoded = encode_direct(&request, None, false);
        let tool_choice = encoded
            .body
            .get("tool_choice")
            .expect("json schema response format should force synthetic tool");
        assert_eq!(tool_choice["type"], "tool");
        assert_eq!(tool_choice["name"], SYNTHETIC_TOOL_NAME);
        assert!(
            encoded.body.get("thinking").is_none(),
            "forced tool calls must omit thinking"
        );
        assert!(
            encoded.body.get("output_config").is_none(),
            "forced tool calls must omit output_config effort"
        );
    }

    #[test]
    fn build_request_omits_thinking_for_explicit_named_tool_choice() {
        let request = Request {
            tools: Some(vec![ToolDefinition {
                name:        "json_output".to_string(),
                description: "Output JSON".to_string(),
                parameters:  serde_json::json!({"type": "object"}),
            }]),
            tool_choice: Some(ToolChoice::Named {
                tool_name: "json_output".to_string(),
            }),
            provider_options: Some(serde_json::json!({
                "anthropic": {
                    "thinking": {"type": "adaptive"}
                }
            })),
            ..make_base_request()
        };

        let encoded = encode_direct(&request, None, false);
        let tool_choice = encoded
            .body
            .get("tool_choice")
            .expect("named tool choice should be translated");
        assert_eq!(tool_choice["type"], "tool");
        assert_eq!(tool_choice["name"], "json_output");
        assert!(
            encoded.body.get("thinking").is_none(),
            "forced named tool choice must omit explicit thinking"
        );
    }

    #[test]
    fn build_request_omits_effort_for_required_tool_choice() {
        let request = Request {
            model: "claude-opus-4-7".to_string(),
            tools: Some(vec![ToolDefinition {
                name:        "json_output".to_string(),
                description: "Output JSON".to_string(),
                parameters:  serde_json::json!({"type": "object"}),
            }]),
            tool_choice: Some(ToolChoice::Required),
            reasoning_effort: Some(ReasoningEffort::Medium),
            ..make_base_request()
        };

        let encoded = encode_direct(&request, None, false);
        let tool_choice = encoded
            .body
            .get("tool_choice")
            .expect("required tool choice should be translated");
        assert_eq!(tool_choice["type"], "any");
        assert!(
            encoded.body.get("output_config").is_none(),
            "required tool choice must omit output_config effort"
        );
    }

    #[test]
    fn build_request_omits_output_config_when_no_reasoning_effort() {
        let request = make_base_request();
        let encoded = encode_direct(&request, None, false);
        assert!(encoded.body.get("output_config").is_none());
    }

    #[test]
    fn build_request_sets_speed() {
        let request = Request {
            speed: Some(Speed::Fast),
            ..make_base_request()
        };

        let encoded = encode_direct(&request, None, false);
        assert_eq!(encoded.body["speed"], "fast");
    }

    #[test]
    fn build_request_serializes_absent_stop_sequences_as_empty_array() {
        let request = make_base_request();
        let encoded = encode_direct(&request, None, false);
        assert_eq!(encoded.body["stop_sequences"], serde_json::json!([]));
    }

    #[test]
    fn build_request_injects_fast_mode_beta_header() {
        let request = Request {
            speed: Some(Speed::Fast),
            ..make_base_request()
        };

        let encoded = encode_direct(&request, None, false);
        let beta = header_value(&encoded, "anthropic-beta")
            .expect("anthropic-beta header should be present");
        assert!(
            beta.contains(FAST_MODE_BETA_HEADER),
            "beta header should contain fast-mode header, got: {beta}"
        );
    }

    #[test]
    fn build_request_falls_back_to_thinking_budget_for_non_effort_model() {
        let catalog = catalog_with_anthropic_model("");
        let request = Request {
            model: "test-claude".to_string(),
            max_tokens: Some(16_000),
            reasoning_effort: Some(ReasoningEffort::XHigh),
            ..make_base_request()
        };

        let encoded = encode_direct(&request, Some(&catalog), false);
        assert!(
            encoded.body.get("output_config").is_none(),
            "non-effort models must not receive output_config"
        );
        let thinking = encoded
            .body
            .get("thinking")
            .expect("thinking must be set for fallback path");
        assert_eq!(thinking["type"], "enabled");
        assert_eq!(thinking["budget_tokens"], 14_000);
    }

    // --- count_tokens encoding -----------------------------------------------

    #[test]
    fn count_request_omits_generation_only_fields_for_reasoning_effort() {
        let catalog = catalog_with_anthropic_model(
            r#"
reasoning_effort = "levels"
"#,
        );
        let request = Request {
            model: "test-claude".to_string(),
            reasoning_effort: Some(ReasoningEffort::High),
            temperature: Some(0.2),
            top_p: Some(0.9),
            metadata: Some(std::collections::HashMap::from([(
                "trace".to_string(),
                "abc".to_string(),
            )])),
            ..make_base_request()
        };

        // The full request carries generation-only fields...
        let full = encode_direct(&request, Some(&catalog), false);
        assert!(full.body.get("output_config").is_some());

        // ...but the count request strips them.
        let count = encode_count_direct(&request, Some(&catalog));
        assert!(count.body.get("output_config").is_none());
        assert!(count.body.get("max_tokens").is_none());
        assert!(count.body.get("temperature").is_none());
        assert!(count.body.get("top_p").is_none());
        assert!(count.body.get("metadata").is_none());
        assert!(count.body.get("stream").is_none());
    }

    #[test]
    fn count_request_includes_explicit_thinking_when_translated_request_has_it() {
        let request = Request {
            provider_options: Some(serde_json::json!({
                "anthropic": {
                    "thinking": {"type": "enabled", "budget_tokens": 1024}
                }
            })),
            ..make_base_request()
        };

        let count = encode_count_direct(&request, None);
        assert_eq!(count.body["thinking"]["type"], "enabled");
        assert_eq!(count.body["thinking"]["budget_tokens"], 1024);
    }
}
