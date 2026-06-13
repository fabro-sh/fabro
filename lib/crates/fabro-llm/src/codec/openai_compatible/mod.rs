//! The OpenAI Chat Completions (`/chat/completions`) codec.
//!
//! Serves every "OpenAI-compatible" route (kimi, zai, minimax, venice,
//! inception, ollama, litellm, …). Pure translation: no HTTP, auth, or base
//! URL — the adapter shell owns those. Count-tokens and error mapping use the
//! `Codec` trait defaults (this dialect has no count route and uses the shared
//! HTTP-status error mapping).

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
        Ok(request::encode(ctx, stream))
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
}
