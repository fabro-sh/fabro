//! The Gemini `generateContent` codec.
//!
//! Pure translation: no HTTP, auth, or base URL — the adapter shell owns
//! those. The codec is distinctive in two ways: it fully forms its endpoints
//! (model-in-path plus `?alt=sse` for streaming), and it overrides
//! `decode_error` to map Gemini's gRPC status codes out of error bodies
//! (falling back to the HTTP status). Tool-call ids are synthetic UUIDs —
//! Gemini keys `functionResponse` on the function *name*, recovered via an
//! id→name map built from the request's assistant turns.

mod decode;
mod encode;
mod stream;
mod wire;

use crate::codec::{Codec, CodecCtx, EncodedRequest, StreamDecoder, parse_error_body};
use crate::error::Error;
use crate::types::{RateLimitInfo, Response};

/// Codec for the Gemini `generateContent` wire dialect.
pub(crate) struct GeminiGenerate;

impl Codec for GeminiGenerate {
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
        Box::new(stream::SseAccumulator::new(ctx, rate_limit))
    }

    fn encode_count_tokens(&self, ctx: &CodecCtx<'_>) -> Option<Result<EncodedRequest, Error>> {
        Some(Ok(encode::encode_count_tokens(ctx)))
    }

    fn decode_count_tokens(&self, body: &str) -> Result<i64, Error> {
        decode::decode_count_tokens(body)
    }

    /// Gemini errors carry a gRPC status in the body's `status` field; map it
    /// when present, falling back to the HTTP status code.
    fn decode_error(
        &self,
        status: u16,
        body: &str,
        ctx: &CodecCtx<'_>,
        retry_after: Option<f64>,
    ) -> Error {
        let (msg, code, raw) = parse_error_body(body, "status");
        decode::gemini_error(status, msg, ctx.provider_name, code, raw, retry_after)
    }
}
