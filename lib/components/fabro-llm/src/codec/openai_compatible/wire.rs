//! Serde types mirroring the OpenAI Chat Completions wire shapes.

use crate::codec::cache::CacheControl;
use crate::codec::split_inclusive_token_total;
use crate::types::{ContentPart, ReasoningEffort, TokenCounts};

#[derive(serde::Serialize)]
pub(super) struct ApiRequest {
    pub model:            String,
    pub messages:         Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature:      Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens:       Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p:            Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop:             Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools:            Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice:      Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format:  Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream:           Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options:   Option<StreamOptions>,
}

/// Streaming options. Chat Completions only emits the trailing usage chunk
/// when the request opts in, so without this a streamed response reports zero
/// tokens and costs are estimated at $0.
#[derive(serde::Serialize)]
pub(super) struct StreamOptions {
    pub include_usage: bool,
}

#[derive(serde::Serialize)]
pub(super) struct ChatMessage {
    pub role:              String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content:           Option<ChatContent>,
    /// Reasoning/thinking content echoed back for providers that require it
    /// during tool-call continuations (including Kimi and DeepSeek).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id:      Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls:        Option<Vec<ChatToolCall>>,
}

/// Message content: plain text, or text parts when a part carries a
/// `cache_control` breakpoint (aggregators fronting Anthropic models forward
/// it upstream). Unmarked messages keep the plain-string form for maximum
/// compatibility with strict Chat Completions servers.
#[derive(serde::Serialize)]
#[serde(untagged)]
pub(super) enum ChatContent {
    Text(String),
    Parts(Vec<ChatTextPart>),
}

#[derive(serde::Serialize)]
pub(super) struct ChatTextPart {
    #[serde(rename = "type")]
    pub kind:          String,
    pub text:          String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl ChatContent {
    /// Plain-text view for assertions.
    #[cfg(test)]
    pub(super) fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text.as_str()),
            Self::Parts(_) => None,
        }
    }

    /// Mark this content as a prompt-cache breakpoint, converting to parts
    /// form so the annotation has somewhere to live.
    pub(super) fn mark_cache_breakpoint(&mut self) {
        match self {
            Self::Text(text) => {
                *self = Self::Parts(vec![ChatTextPart {
                    kind:          "text".to_string(),
                    text:          std::mem::take(text),
                    cache_control: Some(CacheControl::ephemeral()),
                }]);
            }
            Self::Parts(parts) => {
                if let Some(last) = parts.last_mut() {
                    last.cache_control = Some(CacheControl::ephemeral());
                }
            }
        }
    }
}

#[derive(serde::Serialize)]
pub(super) struct ChatToolCall {
    pub id:       String,
    #[serde(rename = "type")]
    pub kind:     String,
    pub function: ChatFunction,
}

#[derive(serde::Serialize)]
pub(super) struct ChatFunction {
    pub name:      String,
    pub arguments: String,
}

// --- Response types (non-streaming) ---

#[derive(serde::Deserialize)]
pub(super) struct ApiResponse {
    pub id:      String,
    pub model:   String,
    pub choices: Vec<ApiChoice>,
    pub usage:   Option<ApiUsage>,
}

#[derive(serde::Deserialize)]
pub(super) struct ApiChoice {
    pub message:       ApiChoiceMessage,
    pub finish_reason: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct ApiChoiceMessage {
    pub content:           Option<String>,
    pub reasoning_content: Option<String>,
    /// OpenRouter's normalized spelling for reasoning text.
    pub reasoning:         Option<String>,
    /// Structured reasoning channel (OpenRouter and compatible
    /// aggregators). Kept as an untyped value so unknown detail variants
    /// cannot fail an otherwise valid completion.
    #[serde(default)]
    pub reasoning_details: Option<serde_json::Value>,
    pub tool_calls:        Option<Vec<ApiToolCall>>,
}

impl ApiChoiceMessage {
    pub(super) fn reasoning(&self) -> Option<&str> {
        self.reasoning_content
            .as_deref()
            .or(self.reasoning.as_deref())
    }
}

/// Structured `reasoning_details` entries accumulated in wire order.
///
/// The entries are preserved verbatim as an opaque content part so
/// encrypted material survives for future provider-aware replay; only known
/// readable members are ever normalized out of them.
#[derive(Default)]
pub(super) struct ReasoningDetails {
    entries: Vec<serde_json::Value>,
}

impl ReasoningDetails {
    /// Preserve a complete-response `reasoning_details` payload.
    ///
    /// Providers document an array of detail objects; a lone object is
    /// accepted as a single entry. Complete entries retain their received
    /// order and shape; scalars carry nothing replayable and are dropped.
    pub(super) fn from_complete_payload(payload: serde_json::Value) -> Self {
        let entries = match payload {
            serde_json::Value::Array(entries) => entries
                .into_iter()
                .filter(serde_json::Value::is_object)
                .collect(),
            payload @ serde_json::Value::Object(_) => vec![payload],
            _ => Vec::new(),
        };
        Self { entries }
    }

    /// Absorb one streamed `reasoning_details` payload.
    ///
    /// Fragments carrying the same `type` and `index` are coalesced even when
    /// other logical details appear between them. Without an index, a fragment
    /// continues the most recently seen detail of the same type. First-seen
    /// detail order is retained.
    pub(super) fn push_stream_payload(&mut self, payload: serde_json::Value) {
        let incoming = match payload {
            serde_json::Value::Array(entries) => entries,
            payload @ serde_json::Value::Object(_) => vec![payload],
            _ => Vec::new(),
        };
        for entry in incoming {
            if !entry.is_object() {
                continue;
            }
            match self
                .entries
                .iter_mut()
                .rev()
                .find(|existing| continues_detail(existing, &entry))
            {
                Some(existing) => merge_detail_fragment(existing, entry),
                _ => self.entries.push(entry),
            }
        }
    }

    /// Opaque content part holding the accumulated entries, or `None` when
    /// nothing usable arrived.
    pub(super) fn into_content_part(self) -> Option<ContentPart> {
        (!self.entries.is_empty()).then(|| ContentPart::Other {
            kind: ContentPart::OPENAI_COMPAT_REASONING_DETAILS.to_string(),
            data: serde_json::Value::Array(self.entries),
        })
    }
}

/// Text-bearing members whose fragments concatenate across stream chunks.
const DETAIL_TEXT_MEMBERS: [&str; 3] = ["text", "summary", "data"];

/// Whether `entry` continues the logical detail already in `last`.
///
/// Aggregators tag each logical detail with a stable `type` and `index`;
/// fragment streams that omit `index` are matched on `type` alone.
fn continues_detail(last: &serde_json::Value, entry: &serde_json::Value) -> bool {
    let (Some(last_type), Some(entry_type)) = (
        last.get("type").and_then(serde_json::Value::as_str),
        entry.get("type").and_then(serde_json::Value::as_str),
    ) else {
        return false;
    };
    if last_type != entry_type {
        return false;
    }

    match (
        last.get("index").and_then(serde_json::Value::as_u64),
        entry.get("index").and_then(serde_json::Value::as_u64),
    ) {
        (Some(last_index), Some(entry_index)) => last_index == entry_index,
        _ => true,
    }
}

/// Append `entry`'s text fragments onto `last` and fill in members `last`
/// has not seen yet.
fn merge_detail_fragment(last: &mut serde_json::Value, entry: serde_json::Value) {
    let serde_json::Value::Object(entry_members) = entry else {
        return;
    };
    let Some(last_members) = last.as_object_mut() else {
        return;
    };
    for (key, value) in entry_members {
        match last_members.get_mut(&key) {
            Some(serde_json::Value::String(existing))
                if DETAIL_TEXT_MEMBERS.contains(&key.as_str()) =>
            {
                if let Some(fragment) = value.as_str() {
                    existing.push_str(fragment);
                }
            }
            Some(_) => {}
            None => {
                last_members.insert(key, value);
            }
        }
    }
}

#[derive(serde::Deserialize)]
pub(super) struct ApiToolCall {
    pub id:       String,
    pub function: ApiFunction,
}

#[derive(serde::Deserialize)]
pub(super) struct ApiFunction {
    pub name:      String,
    pub arguments: String,
}

#[derive(serde::Deserialize)]
#[allow(
    clippy::struct_field_names,
    reason = "Field names mirror the provider API payload."
)]
pub(super) struct ApiUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    /// Tolerant superset: aggregator dialects (OpenRouter) report in-band
    /// USD cost and cache/reasoning token detail. Absent on plain providers.
    #[serde(default)]
    pub cost: Option<f64>,
    #[serde(default)]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
    /// DeepSeek-specific top-level count of prompt tokens served from its
    /// automatic context cache.
    #[serde(default)]
    pub prompt_cache_hit_tokens: Option<i64>,
    #[serde(default)]
    pub completion_tokens_details: Option<CompletionTokensDetails>,
    /// Modal reports reasoning tokens directly on `usage` instead of nesting
    /// them under `completion_tokens_details`.
    #[serde(default)]
    pub reasoning_tokens: Option<i64>,
}

#[derive(serde::Deserialize)]
pub(super) struct PromptTokensDetails {
    #[serde(default)]
    pub cached_tokens:      Option<i64>,
    /// OpenRouter-specific: explicit-cache write tokens.
    #[serde(default)]
    pub cache_write_tokens: Option<i64>,
}

#[derive(serde::Deserialize)]
pub(super) struct CompletionTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: Option<i64>,
}

impl ApiUsage {
    /// Normalize into disjoint [`TokenCounts`] buckets: cached and
    /// cache-write detail tokens are subtracted out of `input_tokens`, and
    /// reasoning tokens out of `output_tokens`, mirroring the
    /// `openai_responses` convention.
    ///
    /// Nested detail fields win over the flat `prompt_cache_hit_tokens` and
    /// `reasoning_tokens` spellings that some providers send instead.
    pub(super) fn token_counts(&self) -> TokenCounts {
        let cached_detail = self
            .prompt_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens)
            .or(self.prompt_cache_hit_tokens)
            .unwrap_or(0);
        let cache_write_detail = self
            .prompt_tokens_details
            .as_ref()
            .and_then(|d| d.cache_write_tokens)
            .unwrap_or(0);
        let reasoning_detail = self
            .completion_tokens_details
            .as_ref()
            .and_then(|d| d.reasoning_tokens)
            .or(self.reasoning_tokens)
            .unwrap_or(0);
        let (uncached_input, cached) =
            split_inclusive_token_total(self.prompt_tokens, cached_detail);
        let (input_tokens, cache_write) =
            split_inclusive_token_total(uncached_input, cache_write_detail);
        let (output_tokens, reasoning) =
            split_inclusive_token_total(self.completion_tokens, reasoning_detail);
        TokenCounts {
            input_tokens,
            output_tokens,
            reasoning_tokens: reasoning,
            cache_read_tokens: cached,
            cache_write_tokens: cache_write,
        }
    }
}

// --- Streaming response types ---

#[derive(serde::Deserialize)]
pub(super) struct StreamChunk {
    pub id:      Option<String>,
    pub model:   Option<String>,
    pub choices: Option<Vec<StreamChoice>>,
    pub usage:   Option<ApiUsage>,
    /// Top-level in-band provider error. OpenRouter includes this after the
    /// HTTP 200 response has already committed.
    pub error:   Option<serde_json::Value>,
}

#[derive(serde::Deserialize)]
pub(super) struct StreamChoice {
    pub delta:         Option<StreamDelta>,
    pub finish_reason: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct StreamDelta {
    pub content:           Option<String>,
    /// Reasoning/thinking content (used by Kimi and other reasoning models).
    pub reasoning_content: Option<String>,
    /// OpenRouter's normalized spelling for reasoning text.
    pub reasoning:         Option<String>,
    /// Structured reasoning channel, streamed as fragments of the entries
    /// the non-streaming response returns whole.
    #[serde(default)]
    pub reasoning_details: Option<serde_json::Value>,
    pub tool_calls:        Option<Vec<StreamToolCall>>,
}

impl StreamDelta {
    pub(super) fn reasoning(&self) -> Option<&str> {
        self.reasoning_content
            .as_deref()
            .or(self.reasoning.as_deref())
    }
}

#[derive(serde::Deserialize)]
pub(super) struct StreamToolCall {
    pub index:    usize,
    pub id:       Option<String>,
    pub function: Option<StreamFunction>,
}

#[derive(serde::Deserialize)]
pub(super) struct StreamFunction {
    pub name:      Option<String>,
    pub arguments: Option<String>,
}

// --- Accumulated tool call state for streaming ---

pub(super) struct AccumulatedToolCall {
    pub id:        String,
    pub name:      String,
    pub arguments: String,
    pub started:   bool,
}

#[cfg(test)]
mod tests {
    use super::{
        ApiResponse, ApiUsage, ChatContent, ChatTextPart, ReasoningDetails, StreamChunk,
        continues_detail,
    };
    use crate::codec::cache::CacheControl;
    use crate::types::{ContentPart, TokenCounts};

    #[test]
    fn reasoning_detail_continuation_uses_type_when_either_index_is_missing() {
        let indexed = serde_json::json!({"type": "reasoning.text", "index": 0});
        let unindexed = serde_json::json!({"type": "reasoning.text"});

        assert!(continues_detail(&indexed, &unindexed));
        assert!(continues_detail(&unindexed, &indexed));
        assert!(continues_detail(&unindexed, &unindexed));
    }

    #[test]
    fn reasoning_detail_continuation_requires_a_matching_string_type() {
        let detail = serde_json::json!({"type": "reasoning.text", "index": 0});

        assert!(!continues_detail(
            &detail,
            &serde_json::json!({"type": "reasoning.summary", "index": 0})
        ));
        assert!(!continues_detail(
            &serde_json::json!({"index": 0}),
            &serde_json::json!({"index": 0})
        ));
        assert!(!continues_detail(
            &serde_json::json!({"type": 7, "index": 0}),
            &serde_json::json!({"type": 7, "index": 0})
        ));
    }

    #[test]
    fn unindexed_reasoning_fragment_continues_the_latest_matching_type() {
        let mut details = ReasoningDetails::default();
        details.push_stream_payload(serde_json::json!([
            {"type": "reasoning.text", "text": "first", "index": 0},
            {"type": "reasoning.text", "text": "second", "index": 1},
        ]));
        details.push_stream_payload(serde_json::json!([
            {"type": "reasoning.text", "text": " continued"},
        ]));

        let ContentPart::Other { data, .. } =
            details.into_content_part().expect("reasoning detail part")
        else {
            panic!("expected opaque reasoning detail part");
        };
        assert_eq!(
            data,
            serde_json::json!([
                {"type": "reasoning.text", "text": "first", "index": 0},
                {"type": "reasoning.text", "text": "second continued", "index": 1},
            ])
        );
    }

    #[test]
    fn chat_content_text_serializes_as_plain_string() {
        let content = ChatContent::Text("Hello".to_string());
        assert_eq!(
            serde_json::to_value(&content).unwrap(),
            serde_json::json!("Hello")
        );
    }

    #[test]
    fn mark_cache_breakpoint_converts_text_to_annotated_parts() {
        let mut content = ChatContent::Text("Hello".to_string());
        content.mark_cache_breakpoint();
        assert_eq!(
            serde_json::to_value(&content).unwrap(),
            serde_json::json!([{
                "type": "text",
                "text": "Hello",
                "cache_control": {"type": "ephemeral"}
            }])
        );
    }

    #[test]
    fn mark_cache_breakpoint_annotates_last_existing_part() {
        let mut content = ChatContent::Parts(vec![
            ChatTextPart {
                kind:          "text".to_string(),
                text:          "first".to_string(),
                cache_control: None,
            },
            ChatTextPart {
                kind:          "text".to_string(),
                text:          "second".to_string(),
                cache_control: Some(CacheControl::ephemeral()),
            },
        ]);
        content.mark_cache_breakpoint();
        let json = serde_json::to_value(&content).unwrap();
        assert!(json[0].get("cache_control").is_none());
        assert_eq!(json[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn token_counts_bound_detail_to_parent_totals() {
        let usage: ApiUsage = serde_json::from_value(serde_json::json!({
            "prompt_tokens": 53,
            "completion_tokens": 59,
            "completion_tokens_details": {"reasoning_tokens": 66}
        }))
        .unwrap();

        assert_eq!(usage.token_counts(), TokenCounts {
            input_tokens: 53,
            output_tokens: 0,
            reasoning_tokens: 59,
            ..TokenCounts::default()
        });
    }

    #[test]
    fn token_counts_accept_deepseek_cache_hit_field() {
        let usage: ApiUsage = serde_json::from_value(serde_json::json!({
            "prompt_tokens": 53,
            "completion_tokens": 11,
            "prompt_cache_hit_tokens": 41
        }))
        .unwrap();

        assert_eq!(usage.token_counts(), TokenCounts {
            input_tokens: 12,
            output_tokens: 11,
            cache_read_tokens: 41,
            ..TokenCounts::default()
        });
    }

    #[test]
    fn token_counts_accept_modal_reasoning_tokens_field() {
        let usage: ApiUsage = serde_json::from_value(serde_json::json!({
            "prompt_tokens": 116,
            "completion_tokens": 66,
            "reasoning_tokens": 54
        }))
        .unwrap();

        assert_eq!(usage.token_counts(), TokenCounts {
            input_tokens: 116,
            output_tokens: 12,
            reasoning_tokens: 54,
            ..TokenCounts::default()
        });
    }

    #[test]
    fn token_counts_prefer_nested_reasoning_detail_over_top_level() {
        let both_spellings: ApiUsage = serde_json::from_value(serde_json::json!({
            "prompt_tokens": 10,
            "completion_tokens": 66,
            "completion_tokens_details": {"reasoning_tokens": 20},
            "reasoning_tokens": 54
        }))
        .unwrap();
        assert_eq!(both_spellings.token_counts().reasoning_tokens, 20);

        let empty_detail: ApiUsage = serde_json::from_value(serde_json::json!({
            "prompt_tokens": 10,
            "completion_tokens": 66,
            "completion_tokens_details": {},
            "reasoning_tokens": 54
        }))
        .unwrap();
        assert_eq!(empty_detail.token_counts().reasoning_tokens, 54);
    }

    #[test]
    fn reasoning_accepts_provider_and_openrouter_spellings() {
        let provider_response: ApiResponse = serde_json::from_value(serde_json::json!({
            "id": "response-1",
            "model": "reasoning-model",
            "choices": [{
                "message": {
                    "content": null,
                    "reasoning_content": "provider reasoning"
                },
                "finish_reason": "stop"
            }]
        }))
        .unwrap();
        assert_eq!(
            provider_response.choices[0].message.reasoning(),
            Some("provider reasoning")
        );

        let openrouter_response: ApiResponse = serde_json::from_value(serde_json::json!({
            "id": "response-2",
            "model": "reasoning-model",
            "choices": [{
                "message": {
                    "content": null,
                    "reasoning": "OpenRouter reasoning"
                },
                "finish_reason": "stop"
            }]
        }))
        .unwrap();
        assert_eq!(
            openrouter_response.choices[0].message.reasoning(),
            Some("OpenRouter reasoning")
        );
        let openrouter_chunk: StreamChunk = serde_json::from_value(serde_json::json!({
            "id": "response-2",
            "model": "reasoning-model",
            "choices": [{
                "delta": {"reasoning": "OpenRouter reasoning"},
                "finish_reason": null
            }]
        }))
        .unwrap();
        assert_eq!(
            openrouter_chunk.choices.unwrap()[0]
                .delta
                .as_ref()
                .unwrap()
                .reasoning(),
            Some("OpenRouter reasoning")
        );
    }
}
