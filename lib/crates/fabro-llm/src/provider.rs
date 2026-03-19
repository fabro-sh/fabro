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
    Inception,
}

impl Provider {
    /// All known provider variants, for use in guardrail tests and iteration.
    pub const ALL: &[Provider] = &[
        Provider::Anthropic,
        Provider::OpenAi,
        Provider::Gemini,
        Provider::Kimi,
        Provider::Zai,
        Provider::Minimax,
        Provider::Inception,
    ];

    /// Environment variable names that can provide the API key for this provider.
    /// Gemini accepts either `GEMINI_API_KEY` or `GOOGLE_API_KEY`.
    #[must_use]
    pub fn api_key_env_vars(self) -> &'static [&'static str] {
        match self {
            Self::Anthropic => &["ANTHROPIC_API_KEY"],
            Self::OpenAi => &["OPENAI_API_KEY"],
            Self::Gemini => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
            Self::Kimi => &["KIMI_API_KEY"],
            Self::Zai => &["ZAI_API_KEY"],
            Self::Minimax => &["MINIMAX_API_KEY"],
            Self::Inception => &["INCEPTION_API_KEY"],
        }
    }

    /// Returns `true` if at least one of the provider's API key env vars is set.
    #[must_use]
    pub fn has_api_key(self) -> bool {
        self.api_key_env_vars()
            .iter()
            .any(|var| std::env::var(var).is_ok())
    }

    /// Pick the best default provider based on which API keys are available.
    ///
    /// Checks Anthropic → OpenAI → Gemini; falls back to Anthropic if none
    /// have a key configured.
    #[must_use]
    pub fn default_from_env() -> Self {
        Self::default_with(Self::has_api_key)
    }

    /// Testable core of [`default_from_env`]: walks the precedence list and
    /// returns the first provider for which `is_configured` returns `true`.
    fn default_with(is_configured: impl Fn(Self) -> bool) -> Self {
        const PRECEDENCE: [Provider; 3] = [Provider::Anthropic, Provider::OpenAi, Provider::Gemini];
        PRECEDENCE
            .iter()
            .copied()
            .find(|&p| is_configured(p))
            .unwrap_or(Provider::Anthropic)
    }

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
            Self::Inception => "inception",
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
            "inception" | "inception_labs" => Ok(Self::Inception),
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
pub type StreamEventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, SdkError>> + Send>>;

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

    #[test]
    fn parse_inception() {
        assert_eq!(
            "inception".parse::<Provider>().unwrap(),
            Provider::Inception
        );
        assert_eq!(
            "inception_labs".parse::<Provider>().unwrap(),
            Provider::Inception
        );
    }

    #[test]
    fn inception_as_str() {
        assert_eq!(Provider::Inception.as_str(), "inception");
    }

    #[test]
    fn default_with_all_configured_prefers_anthropic() {
        assert_eq!(Provider::default_with(|_| true), Provider::Anthropic);
    }

    #[test]
    fn default_with_only_openai() {
        assert_eq!(
            Provider::default_with(|p| p == Provider::OpenAi),
            Provider::OpenAi
        );
    }

    #[test]
    fn default_with_only_gemini() {
        assert_eq!(
            Provider::default_with(|p| p == Provider::Gemini),
            Provider::Gemini
        );
    }

    #[test]
    fn default_with_openai_and_gemini_prefers_openai() {
        assert_eq!(
            Provider::default_with(|p| p == Provider::OpenAi || p == Provider::Gemini),
            Provider::OpenAi,
        );
    }

    #[test]
    fn default_with_none_configured_falls_back_to_anthropic() {
        assert_eq!(Provider::default_with(|_| false), Provider::Anthropic);
    }

    #[test]
    fn default_with_only_kimi_falls_back_to_anthropic() {
        assert_eq!(
            Provider::default_with(|p| p == Provider::Kimi),
            Provider::Anthropic
        );
    }

    #[test]
    fn api_key_env_vars_anthropic() {
        assert_eq!(
            Provider::Anthropic.api_key_env_vars(),
            &["ANTHROPIC_API_KEY"]
        );
    }

    #[test]
    fn api_key_env_vars_openai() {
        assert_eq!(Provider::OpenAi.api_key_env_vars(), &["OPENAI_API_KEY"]);
    }

    #[test]
    fn api_key_env_vars_gemini_has_two() {
        let vars = Provider::Gemini.api_key_env_vars();
        assert_eq!(vars.len(), 2);
        assert_eq!(vars, &["GEMINI_API_KEY", "GOOGLE_API_KEY"]);
    }

    #[test]
    fn api_key_env_vars_kimi() {
        assert_eq!(Provider::Kimi.api_key_env_vars(), &["KIMI_API_KEY"]);
    }

    #[test]
    fn api_key_env_vars_zai() {
        assert_eq!(Provider::Zai.api_key_env_vars(), &["ZAI_API_KEY"]);
    }

    #[test]
    fn api_key_env_vars_minimax() {
        assert_eq!(Provider::Minimax.api_key_env_vars(), &["MINIMAX_API_KEY"]);
    }

    #[test]
    fn api_key_env_vars_inception() {
        assert_eq!(
            Provider::Inception.api_key_env_vars(),
            &["INCEPTION_API_KEY"]
        );
    }

    #[test]
    fn every_provider_has_at_least_one_env_var() {
        assert!(Provider::ALL
            .iter()
            .all(|p| !p.api_key_env_vars().is_empty()));
    }

    // Mock adapter that supports all tool choices
    struct MockAdapter;

    #[async_trait::async_trait]
    impl ProviderAdapter for MockAdapter {
        fn name(&self) -> &str {
            "mock"
        }
        async fn complete(&self, _request: &Request) -> Result<Response, SdkError> {
            unimplemented!()
        }
        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, SdkError> {
            unimplemented!()
        }
    }

    // Mock adapter that rejects "named" tool choice
    struct RestrictedAdapter;

    #[async_trait::async_trait]
    impl ProviderAdapter for RestrictedAdapter {
        fn name(&self) -> &str {
            "restricted"
        }
        async fn complete(&self, _request: &Request) -> Result<Response, SdkError> {
            unimplemented!()
        }
        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, SdkError> {
            unimplemented!()
        }
        fn supports_tool_choice(&self, mode: &str) -> bool {
            mode != "named"
        }
    }

    #[test]
    fn validate_tool_choice_auto_accepted() {
        assert!(validate_tool_choice(&MockAdapter, &ToolChoice::Auto).is_ok());
    }

    #[test]
    fn validate_tool_choice_none_accepted() {
        assert!(validate_tool_choice(&MockAdapter, &ToolChoice::None).is_ok());
    }

    #[test]
    fn validate_tool_choice_required_accepted() {
        assert!(validate_tool_choice(&MockAdapter, &ToolChoice::Required).is_ok());
    }

    #[test]
    fn validate_tool_choice_named_rejected_by_restricted() {
        let result = validate_tool_choice(&RestrictedAdapter, &ToolChoice::named("my_tool"));
        assert!(result.is_err());
        match result.unwrap_err() {
            SdkError::UnsupportedToolChoice { message } => {
                assert!(message.contains("restricted"));
                assert!(message.contains("named"));
            }
            other => panic!("expected UnsupportedToolChoice, got {other:?}"),
        }
    }

    #[test]
    fn validate_tool_choice_named_accepted_by_default() {
        assert!(validate_tool_choice(&MockAdapter, &ToolChoice::named("my_tool")).is_ok());
    }
}
