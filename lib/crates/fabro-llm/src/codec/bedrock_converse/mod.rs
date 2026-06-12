//! The Amazon Bedrock Converse codec.
//!
//! Pure translation: no HTTP, auth, signing, or event-stream framing — the
//! Bedrock adapter shell owns those. Converse is Bedrock's model-agnostic
//! envelope (AWS translates it to each hosted family's native dialect
//! server-side), which is what makes this one codec serve Claude, Nova,
//! Llama, Mistral, DeepSeek, Qwen, Kimi, GLM, MiniMax, Nemotron, and
//! gpt-oss alike. The codec fully forms its endpoints (model-in-path,
//! `/converse` vs `/converse-stream`), mirrors the anthropic codec's prompt
//! cache placement with `cachePoint` blocks, and round-trips
//! `reasoningContent` thinking signatures unmodified.

mod decode;
mod encode;
mod stream;

use crate::codec::{Codec, CodecCtx, EncodedRequest, StreamDecoder};
use crate::error::Error;
use crate::types::{RateLimitInfo, Response};

/// Codec for the Bedrock Converse wire dialect.
pub(crate) struct BedrockConverse;

impl Codec for BedrockConverse {
    fn encode(&self, ctx: &CodecCtx<'_>, stream: bool) -> Result<EncodedRequest, Error> {
        encode::encode(ctx, stream)
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
        Box::new(stream::ConverseStreamDecoder::new(ctx, rate_limit))
    }

    /// Bedrock error bodies are AWS-shaped (top-level `message`/`Message`,
    /// `__type`), which the default parser misses — extract them so failures
    /// surface the real reason instead of "Unknown error".
    fn decode_error(
        &self,
        status: u16,
        body: &str,
        ctx: &CodecCtx<'_>,
        retry_after: Option<f64>,
    ) -> Error {
        decode::bedrock_error(status, body, ctx.provider_name, retry_after)
    }
}
