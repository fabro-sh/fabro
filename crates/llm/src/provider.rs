use crate::error::SdkError;
use crate::types::{Request, Response, StreamEvent, ToolChoice};
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::pin::Pin;
use std::str::FromStr;

// ---------------------------------------------------------------------------
// Provider enum — compile-time safe provider identity
// ---------------------------------------------------------------------------

/// Known LLM provider variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Anthropic,
    OpenAi,
    Gemini,
    Kimi,
    Zai,
    Minimax,
}

impl Provider {
    /// Stable lowercase string representation used in `Request.provider`,
    /// adapter names, and other serialization boundaries.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Gemini => "gemini",
            Self::Kimi => "kimi",
            Self::Zai => "zai",
            Self::Minimax => "minimax",
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Provider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "anthropic" => Ok(Self::Anthropic),
            "openai" | "open_ai" => Ok(Self::OpenAi),
            "gemini" => Ok(Self::Gemini),
            "kimi" => Ok(Self::Kimi),
            "zai" => Ok(Self::Zai),
            "minimax" => Ok(Self::Minimax),
            other => Err(format!("unknown provider: {other}")),
        }
    }
}

// ---------------------------------------------------------------------------
// ModelId — bundles a provider with a model name
// ---------------------------------------------------------------------------

/// A model identifier that pairs a [`Provider`] with the provider-specific
/// model name (e.g. `"claude-opus-4-6"` or `"gpt-4o-mini"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelId {
    pub provider: Provider,
    pub model: String,
}

impl ModelId {
    #[must_use]
    pub fn new(provider: Provider, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.provider, self.model)
    }
}

// ---------------------------------------------------------------------------
// ProviderAdapter trait
// ---------------------------------------------------------------------------

/// Async stream of `StreamEvents` returned by streaming providers.
pub type StreamEventStream =
    Pin<Box<dyn Stream<Item = Result<StreamEvent, SdkError>> + Send>>;

/// The contract that every provider adapter must implement (Section 2.4).
#[async_trait::async_trait]
pub trait ProviderAdapter: Send + Sync {
    /// Provider name, e.g. "openai", "anthropic", "gemini"
    fn name(&self) -> &str;

    /// Send a request and block until the model finishes (Section 4.1).
    async fn complete(&self, request: &Request) -> Result<Response, SdkError>;

    /// Send a request and return an async stream of events (Section 4.2).
    async fn stream(&self, request: &Request) -> Result<StreamEventStream, SdkError>;

    /// Release resources. Called by `Client::close()`.
    async fn close(&self) -> Result<(), SdkError> {
        Ok(())
    }

    /// Validate configuration on startup. Called by Client on registration.
    async fn initialize(&self) -> Result<(), SdkError> {
        Ok(())
    }

    /// Query whether a particular tool choice mode is supported.
    fn supports_tool_choice(&self, _mode: &str) -> bool {
        true
    }
}

/// Validate that the adapter supports the requested tool choice mode.
///
/// Returns `Err(SdkError::UnsupportedToolChoice)` if the adapter does not
/// support the given mode.
///
/// # Errors
///
/// Returns `SdkError::UnsupportedToolChoice` when the adapter does not
/// support the requested tool choice mode.
pub fn validate_tool_choice(
    adapter: &dyn ProviderAdapter,
    tool_choice: &ToolChoice,
) -> Result<(), SdkError> {
    let mode = tool_choice.mode_str();
    if !adapter.supports_tool_choice(mode) {
        return Err(SdkError::UnsupportedToolChoice {
            message: format!(
                "provider '{}' does not support tool_choice mode '{mode}'",
                adapter.name()
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kimi() {
        assert_eq!("kimi".parse::<Provider>().unwrap(), Provider::Kimi);
    }

    #[test]
    fn parse_zai() {
        assert_eq!("zai".parse::<Provider>().unwrap(), Provider::Zai);
    }

    #[test]
    fn parse_minimax() {
        assert_eq!("minimax".parse::<Provider>().unwrap(), Provider::Minimax);
    }

    #[test]
    fn kimi_as_str() {
        assert_eq!(Provider::Kimi.as_str(), "kimi");
    }

    #[test]
    fn zai_as_str() {
        assert_eq!(Provider::Zai.as_str(), "zai");
    }

    #[test]
    fn minimax_as_str() {
        assert_eq!(Provider::Minimax.as_str(), "minimax");
    }
}
