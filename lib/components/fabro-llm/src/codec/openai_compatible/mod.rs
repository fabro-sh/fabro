//! The OpenAI Chat Completions (`/chat/completions`) codec.
//!
//! Serves every "OpenAI-compatible" route (moonshot, zai, minimax, venice,
//! inception, ollama, litellm, …). Pure translation: no HTTP, auth, or base
//! URL — the adapter shell owns those. This dialect has no count route. Error
//! mapping also understands the typed provider errors and routing-attempt
//! metadata returned by OpenRouter.

mod error;
mod request;
mod response;
mod stream;
mod translate;
mod wire;

use crate::codec::{Codec, CodecCtx, EncodedRequest, StreamDecoder};
use crate::error::Error;
use crate::types::{RateLimitInfo, Response};

/// Codec for the OpenAI Chat Completions wire dialect.
pub(crate) struct OpenAiCompatible;

impl Codec for OpenAiCompatible {
    fn encode(&self, ctx: &CodecCtx<'_>, stream: bool) -> Result<EncodedRequest, Error> {
        request::encode(ctx, stream)
    }

    fn decode_response(
        &self,
        body: &str,
        ctx: &CodecCtx<'_>,
        rate_limit: Option<RateLimitInfo>,
    ) -> Result<Response, Error> {
        response::decode_response(body, ctx, rate_limit)
    }

    fn stream_decoder(
        &self,
        ctx: &CodecCtx<'_>,
        rate_limit: Option<RateLimitInfo>,
    ) -> Box<dyn StreamDecoder> {
        Box::new(stream::StreamState::new(ctx, rate_limit))
    }

    fn decode_error(
        &self,
        status: u16,
        body: &str,
        ctx: &CodecCtx<'_>,
        retry_after: Option<f64>,
    ) -> Error {
        error::decode_http(status, body, ctx.provider_name, retry_after)
    }
}
