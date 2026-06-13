//! The OpenAI Responses (`/responses`) codec.
//!
//! Serves OpenAI direct today, in two route flavors that share this codec:
//! the standard route and the Codex route (`CodecParams::openai_codex`, which
//! omits sampling params encode-side; its forced streaming lives in the
//! adapter's route config). Pure translation: no HTTP, auth, or base URL —
//! the adapter shell owns those.
//!
//! HTTP error bodies use the shared `decode_error` default (openai uses the
//! standard `error_from_status_code` + `parse_error_body` path); streaming
//! `error` / `response.failed` events are mapped inside the decoder
//! (`on_event` → `Err`).

mod decode;
mod encode;
mod stream;
mod wire;

use crate::codec::{Codec, CodecCtx, EncodedRequest, StreamDecoder};
use crate::error::Error;
use crate::types::{RateLimitInfo, Response};

/// Codec for the OpenAI Responses wire dialect.
pub(crate) struct OpenAiResponses;

impl Codec for OpenAiResponses {
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
}
