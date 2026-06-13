//! The Anthropic Messages (`/messages`) codec.
//!
//! Serves Anthropic direct today, and (via route config + `CodecParams`)
//! Kimi-over-anthropic; the Bedrock and OpenRouter-skin routes pair the same
//! codec with different transports later. Pure translation: no HTTP, auth, or
//! base URL — the adapter shell owns those.
//!
//! HTTP error bodies use the shared `decode_error` default (anthropic uses the
//! standard `error_from_status_code` + `parse_error_body` path); streaming
//! `error` events are mapped inside the decoder (`on_event` → `Err`).

mod decode;
mod encode;
mod stream;
mod wire;

pub(crate) use encode::anthropic_option;

use crate::codec::{Codec, CodecCtx, EncodedRequest, StreamDecoder};
use crate::error::Error;
use crate::types::{RateLimitInfo, Response};

/// Synthetic tool injected to coerce structured (`JsonSchema`) output. Shared
/// across encode (injection), decode (extraction), and stream (rewrite).
pub(super) const SYNTHETIC_TOOL_NAME: &str = "json_output";

/// Codec for the Anthropic Messages wire dialect.
pub(crate) struct AnthropicMessages;

impl Codec for AnthropicMessages {
    fn encode(&self, ctx: &CodecCtx<'_>, stream: bool) -> Result<EncodedRequest, Error> {
        Ok(encode::encode(ctx, stream))
    }

    fn decode_response(
        &self,
        body: &str,
        ctx: &CodecCtx<'_>,
        rate_limit: Option<RateLimitInfo>,
    ) -> Result<Response, Error> {
        decode::decode_response(body, ctx, rate_limit)
    }

    fn stream_decoder(
        &self,
        ctx: &CodecCtx<'_>,
        rate_limit: Option<RateLimitInfo>,
    ) -> Box<dyn StreamDecoder> {
        Box::new(stream::SseAccumulator::new(
            ctx.provider_name,
            decode::uses_json_schema_format(ctx.request),
            rate_limit,
        ))
    }

    fn encode_count_tokens(&self, ctx: &CodecCtx<'_>) -> Option<Result<EncodedRequest, Error>> {
        Some(Ok(encode::encode_count_tokens(ctx)))
    }

    fn decode_count_tokens(&self, body: &str) -> Result<i64, Error> {
        decode::decode_count_tokens(body)
    }
}
