//! The codec seam: pure, sync translation between the canonical core
//! (`Request`/`Response`/`StreamEvent`) and a provider wire dialect.
//!
//! A codec knows *what the bytes say*. It does NOT know how they travel
//! (auth, base URL, retries, streaming transport) — that's the adapter/
//! transport layer. Everything a codec varies on arrives as data in
//! [`CodecCtx`] / [`CodecParams`]; codecs hold no per-request state.
//!
//! The trait is intentionally complete (count-tokens + error mapping have
//! defaults) so the per-dialect codecs that follow only ever *override*
//! methods, never extend the contract.

pub(crate) mod anthropic_messages;
pub(crate) mod gemini_generate;
pub(crate) mod openai_compatible;
pub(crate) mod openai_responses;

use fabro_model::Model;

use crate::error::{Error, error_from_status_code};
use crate::types::{Message, RateLimitInfo, Request, Response, Role, StreamEvent};

/// Per-request context. Borrowed — the codec reads what it needs and returns.
pub(crate) struct CodecCtx<'a> {
    /// The canonical request being translated. Decoders read it too
    /// (e.g. tool-argument parsing keys off the request's tool definitions;
    /// the stream model fallback uses `request.model`).
    pub request:       &'a Request,
    /// Identity stamped into `Response.provider`, and the `provider_options`
    /// namespace key for the openai_compatible codec (kimi/zai/…).
    pub provider_name: &'a str,
    /// The model id to send on the wire — catalog `api_id`, resolved by the
    /// route (today `api_id == id` everywhere).
    pub deployment_id: &'a str,
    /// Model row for capability lookups (prompt_cache, reasoning levels,
    /// max_output). `None` when no catalog is injected.
    pub model:         Option<&'a Model>,
    /// Per-route dialect data (model/version placement, …). Defaulted to
    /// today's direct-route values; Bedrock/OpenRouter add variants later.
    pub params:        &'a CodecParams,
}

/// Per-route dialect knobs, expressed as data so one codec can serve several
/// routes. The default is inert ("nothing special"); a route that needs a
/// dialect quirk sets the relevant field. Grows as codecs need it — #459 adds
/// `ModelPlacement` for Bedrock. Inert for codecs that don't read a given
/// field.
#[derive(Debug, Default, Clone)]
pub(crate) struct CodecParams {
    /// Where/whether to place the Anthropic API version. Direct Anthropic uses
    /// `Header("2023-06-01")`; Kimi-over-anthropic uses `None`; the Bedrock
    /// redo will add a body-field variant. Inert for non-anthropic codecs.
    pub anthropic_version: AnthropicVersion,
    /// Whether to emit Anthropic beta headers (prompt-caching / fast-mode /
    /// 1M-context). True on the direct route, false for Kimi-over-anthropic.
    pub anthropic_beta:    bool,
    /// Codex-endpoint dialect for the openai_responses codec: omit the
    /// sampling params (`temperature`/`top_p`/`max_output_tokens`) the Codex
    /// endpoint rejects and always send `instructions` (empty string when the
    /// request has none). The transport-side half of codex mode (forced
    /// streaming) is route config, not codec data.
    pub openai_codex:      bool,
}

/// Placement of the Anthropic API version on the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum AnthropicVersion {
    /// No version sent (Kimi-over-anthropic; also the inert default).
    #[default]
    None,
    /// `anthropic-version` request header (direct Anthropic).
    Header(&'static str),
    // BodyField(&'static str) arrives with the Bedrock redo (#459).
}

/// What [`Codec::encode`] produces. The transport applies `endpoint` +
/// `headers` on top of the route's base URL and auth; the codec never touches
/// HTTP.
pub(crate) struct EncodedRequest {
    /// Request body.
    pub body:     serde_json::Value,
    /// Path appended to the route base URL, fully formed by the codec
    /// (incl. model-in-path and `?alt=sse` for gemini). e.g.
    /// `/chat/completions`.
    pub endpoint: String,
    /// Dialect headers as data (e.g. `anthropic-version`, beta headers).
    /// NOT auth or `content-type` — those are the transport's job. Empty for
    /// the openai_compatible codec.
    pub headers:  Vec<(String, String)>,
}

/// One framed item off the byte stream, handed to a [`StreamDecoder`].
pub(crate) struct RawEvent<'a> {
    /// SSE `event:` type — `Some` when the framing carries one (anthropic,
    /// openai responses); `None` for the data-only framing
    /// openai_compatible/gemini use.
    pub event: Option<&'a str>,
    /// The `data:` payload, or a bare JSON line. The sentinel `[DONE]` is
    /// passed through verbatim for the decoder to recognize.
    pub data:  &'a str,
}

/// Stateless translator for one wire dialect.
pub(crate) trait Codec: Send + Sync {
    /// Canonical request (`ctx.request`) → wire request. `stream` selects the
    /// streaming shape (`stream: true` in the body, gemini's
    /// `:streamGenerateContent` endpoint). Fallible: attachment/parameter
    /// encoding can reject.
    fn encode(&self, ctx: &CodecCtx<'_>, stream: bool) -> Result<EncodedRequest, Error>;

    /// Wire response body → canonical `Response` (content parts, finish
    /// reason, usage). Each dialect's finish-reason map and usage arithmetic
    /// live here. Stamps `ctx.provider_name` into `Response.provider` and the
    /// transport-parsed `rate_limit` into the response.
    fn decode_response(
        &self,
        body: &str,
        ctx: &CodecCtx<'_>,
        rate_limit: Option<RateLimitInfo>,
    ) -> Result<Response, Error>;

    /// A fresh stateful decoder for one streaming response. `rate_limit` is the
    /// transport-parsed header value to embed in the synthesized `Finish`.
    fn stream_decoder(
        &self,
        ctx: &CodecCtx<'_>,
        rate_limit: Option<RateLimitInfo>,
    ) -> Box<dyn StreamDecoder>;

    /// The third route, if the dialect has one (`/messages/count_tokens`,
    /// `/responses/input_tokens`, `:countTokens`). `None` = the dialect has no
    /// such route. Whether a given *deployment* may use it is a separate
    /// route-level gate (Kimi-over-anthropic) decided before this is called.
    fn encode_count_tokens(&self, _ctx: &CodecCtx<'_>) -> Option<Result<EncodedRequest, Error>> {
        None
    }

    /// Parse the token count out of a count-tokens response. Only called when
    /// [`Codec::encode_count_tokens`] returned `Some`; the default guards the
    /// invariant for codecs without a count route.
    fn decode_count_tokens(&self, _body: &str) -> Result<i64, Error> {
        Err(Error::Configuration {
            message: "codec has no count_tokens route".to_string(),
            source:  None,
        })
    }

    /// Map a non-2xx response to an `Error`. `retry_after` is the
    /// transport-parsed `retry-after` header value in seconds (header parsing
    /// is the transport's job, like `rate_limit` on the decode methods).
    /// Default = shared HTTP-status mapping, which openai_compatible and
    /// anthropic use as-is; a codec overrides when its dialect's error bodies
    /// need more (e.g. gemini's gRPC status).
    fn decode_error(
        &self,
        status: u16,
        body: &str,
        ctx: &CodecCtx<'_>,
        retry_after: Option<f64>,
    ) -> Error {
        let (message, code, raw) = parse_error_body(body, "type");
        error_from_status_code(
            status,
            message,
            ctx.provider_name.to_string(),
            code,
            raw,
            retry_after,
        )
    }
}

/// Stateful per-stream decoder, driven by the shared transport loop.
/// `'static` because it is boxed into the stream's unfold state.
pub(crate) trait StreamDecoder: Send + 'static {
    /// One framed event → zero or more canonical `StreamEvent`s. Returns
    /// `Err` for dialect error events (anthropic `error`, openai
    /// `response.failed`), which the transport yields as a stream error.
    fn on_event(&mut self, ev: RawEvent<'_>) -> Result<Vec<StreamEvent>, Error>;

    /// Byte-stream-end hook. Semantics are per-decoder, not shared:
    ///   anthropic — return nothing (`message_stop` already finished it);
    ///   openai_compatible — synthesize `Finish` iff content started (minimax);
    ///   gemini — synthesize `Finish` unconditionally if not yet finished.
    fn finish(&mut self) -> Vec<StreamEvent>;
}

// --- Dialect-neutral translation helpers
// ---------------------------------------

/// Parse an error response body, extracting the message and error code.
///
/// `error_code_field` is the JSON field name for the error code (e.g. "type" or
/// "status").
#[must_use]
pub(crate) fn parse_error_body(
    body: &str,
    error_code_field: &str,
) -> (String, Option<String>, Option<serde_json::Value>) {
    serde_json::from_str::<serde_json::Value>(body).map_or_else(
        |_| (body.to_string(), None, None),
        |v| {
            let message = v
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(serde_json::Value::as_str)
                // Codex endpoint returns {"detail": "..."} instead of {"error": {"message": "..."}}
                .or_else(|| v.get("detail").and_then(serde_json::Value::as_str))
                .unwrap_or("Unknown error")
                .to_string();
            let error_code = v
                .get("error")
                .and_then(|e| e.get(error_code_field))
                .and_then(serde_json::Value::as_str)
                .map(String::from);
            (message, error_code, Some(v))
        },
    )
}

/// Extract system and developer messages from a message list.
///
/// Returns the joined system prompt and the remaining messages.
/// Per spec, Developer role messages are merged with system messages
/// for Anthropic and Gemini.
#[must_use]
pub(crate) fn extract_system_prompt(messages: &[Message]) -> (Option<String>, Vec<&Message>) {
    let mut system_parts = Vec::new();
    let mut other = Vec::new();
    for msg in messages {
        if msg.role == Role::System || msg.role == Role::Developer {
            let text = msg.text();
            if !text.trim().is_empty() {
                system_parts.push(text);
            }
        } else {
            other.push(msg);
        }
    }
    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n"))
    };
    (system, other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ContentPart;

    // --- parse_error_body ---

    #[test]
    fn parse_error_body_valid_json() {
        let body = r#"{"error":{"message":"rate limited","type":"rate_limit_error"}}"#;
        let (msg, code, raw) = parse_error_body(body, "type");
        assert_eq!(msg, "rate limited");
        assert_eq!(code.as_deref(), Some("rate_limit_error"));
        assert!(raw.is_some());
    }

    #[test]
    fn parse_error_body_missing_error_field() {
        let body = r#"{"status":"fail"}"#;
        let (msg, code, raw) = parse_error_body(body, "type");
        assert_eq!(msg, "Unknown error");
        assert_eq!(code, None);
        assert!(raw.is_some());
    }

    #[test]
    fn parse_error_body_not_json() {
        let body = "Internal Server Error";
        let (msg, code, raw) = parse_error_body(body, "type");
        assert_eq!(msg, "Internal Server Error");
        assert_eq!(code, None);
        assert!(raw.is_none());
    }

    #[test]
    fn parse_error_body_different_code_field() {
        let body = r#"{"error":{"message":"bad","status":"INVALID_ARGUMENT"}}"#;
        let (msg, code, _) = parse_error_body(body, "status");
        assert_eq!(msg, "bad");
        assert_eq!(code.as_deref(), Some("INVALID_ARGUMENT"));
    }

    #[test]
    fn parse_error_body_no_message() {
        let body = r#"{"error":{"type":"server_error"}}"#;
        let (msg, code, _) = parse_error_body(body, "type");
        assert_eq!(msg, "Unknown error");
        assert_eq!(code.as_deref(), Some("server_error"));
    }

    // --- extract_system_prompt ---

    #[test]
    fn extract_system_prompt_no_system() {
        let msgs = vec![Message::user("hello")];
        let (sys, other) = extract_system_prompt(&msgs);
        assert_eq!(sys, None);
        assert_eq!(other.len(), 1);
    }

    #[test]
    fn extract_system_prompt_system_only() {
        let msgs = vec![Message::system("Be helpful"), Message::user("hi")];
        let (sys, other) = extract_system_prompt(&msgs);
        assert_eq!(sys.as_deref(), Some("Be helpful"));
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].role, Role::User);
    }

    #[test]
    fn extract_system_prompt_multiple_system() {
        let msgs = vec![
            Message::system("Rule 1"),
            Message::system("Rule 2"),
            Message::user("hi"),
        ];
        let (sys, other) = extract_system_prompt(&msgs);
        assert_eq!(sys.as_deref(), Some("Rule 1\nRule 2"));
        assert_eq!(other.len(), 1);
    }

    #[test]
    fn extract_system_prompt_developer_role() {
        let dev = Message {
            role:         Role::Developer,
            content:      vec![ContentPart::text("dev instructions")],
            name:         None,
            tool_call_id: None,
        };
        let msgs = vec![dev, Message::user("hi")];
        let (sys, other) = extract_system_prompt(&msgs);
        assert_eq!(sys.as_deref(), Some("dev instructions"));
        assert_eq!(other.len(), 1);
    }

    #[test]
    fn extract_system_prompt_ignores_whitespace_system_and_developer() {
        let dev = Message {
            role:         Role::Developer,
            content:      vec![ContentPart::text(" \n\t ")],
            name:         None,
            tool_call_id: None,
        };
        let msgs = vec![Message::system("   "), dev, Message::user("hi")];
        let (sys, other) = extract_system_prompt(&msgs);
        assert_eq!(sys, None);
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].role, Role::User);
    }

    #[test]
    fn extract_system_prompt_empty() {
        let msgs: Vec<Message> = vec![];
        let (sys, other) = extract_system_prompt(&msgs);
        assert_eq!(sys, None);
        assert!(other.is_empty());
    }
}
