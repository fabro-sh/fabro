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
pub(crate) mod openai_compatible;

use fabro_model::Model;

use crate::error::{Error, error_from_status_code};
use crate::providers::common::parse_error_body;
use crate::types::{RateLimitInfo, Request, Response, StreamEvent};

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
/// `ModelPlacement` for Bedrock.
#[derive(Debug, Default, Clone)]
pub(crate) struct CodecParams {
    /// Where/whether to place the Anthropic API version. Direct Anthropic uses
    /// `Header("2023-06-01")`; Kimi-over-anthropic uses `None`; the Bedrock
    /// redo will add a body-field variant. Inert for non-anthropic codecs.
    pub anthropic_version: AnthropicVersion,
    /// Whether to emit Anthropic beta headers (prompt-caching / fast-mode /
    /// 1M-context). True on the direct route, false for Kimi-over-anthropic.
    pub anthropic_beta:    bool,
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
    /// SSE `event:` type — `Some` for anthropic; `None` for the data-only
    /// framing openai/gemini use.
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
