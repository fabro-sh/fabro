//! Wire-dialect identity shared by the model catalog and LLM route assembly.
//!
//! A codec names *what the bytes say* — the wire dialect a route speaks —
//! independently of the transport/auth scheme named by
//! [`AdapterKind`](crate::AdapterKind). Catalog rows may select a codec
//! explicitly; rows that omit it inherit the adapter's default, which
//! reproduces the historical adapter→dialect fusion exactly.

use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::adapter::AdapterKind;

/// Stable wire-dialect identity for a route.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
    VariantArray,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum CodecKind {
    AnthropicMessages,
    #[serde(rename = "openai_responses")]
    #[strum(to_string = "openai_responses")]
    OpenAiResponses,
    /// The conservative Chat Completions dialect. The name matches today's
    /// `openai_compatible` adapter string; `openai_chat` stays reserved for a
    /// possible future full-proprietary Chat Completions dialect.
    #[serde(rename = "openai_compatible")]
    #[strum(to_string = "openai_compatible")]
    OpenAiCompatible,
    GeminiGenerate,
}

impl CodecKind {
    /// The codec each adapter kind drives when a catalog row does not
    /// configure `codec` explicitly. These defaults reproduce the historical
    /// behavior where the adapter implied the wire dialect.
    #[must_use]
    pub fn default_for(adapter: AdapterKind) -> Self {
        match adapter {
            AdapterKind::Anthropic => Self::AnthropicMessages,
            AdapterKind::OpenAi => Self::OpenAiResponses,
            AdapterKind::Gemini => Self::GeminiGenerate,
            AdapterKind::OpenAiCompatible => Self::OpenAiCompatible,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

impl AsRef<str> for CodecKind {
    fn as_ref(&self) -> &str {
        (*self).as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_kind_round_trips_as_snake_case() {
        for kind in CodecKind::VARIANTS {
            let json = serde_json::to_string(kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            let parsed: CodecKind = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, *kind);
            assert_eq!(kind.as_str().parse::<CodecKind>().unwrap(), *kind);
        }
    }

    #[test]
    fn codec_kind_strings_match_route_vocabulary() {
        for (kind, expected) in [
            (CodecKind::AnthropicMessages, "anthropic_messages"),
            (CodecKind::OpenAiResponses, "openai_responses"),
            (CodecKind::OpenAiCompatible, "openai_compatible"),
            (CodecKind::GeminiGenerate, "gemini_generate"),
        ] {
            assert_eq!(kind.as_str(), expected);
            assert_eq!(kind.to_string(), expected);
        }
    }

    #[test]
    fn adapter_defaults_reproduce_the_historical_fusion() {
        for (adapter, expected) in [
            (AdapterKind::Anthropic, CodecKind::AnthropicMessages),
            (AdapterKind::OpenAi, CodecKind::OpenAiResponses),
            (AdapterKind::Gemini, CodecKind::GeminiGenerate),
            (AdapterKind::OpenAiCompatible, CodecKind::OpenAiCompatible),
        ] {
            assert_eq!(CodecKind::default_for(adapter), expected);
        }
    }
}
