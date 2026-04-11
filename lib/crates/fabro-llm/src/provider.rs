use std::pin::Pin;

pub use fabro_model::{ModelHandle, Provider};
use futures::Stream;

use crate::error::SdkError;
use crate::types::{Request, Response, StreamEvent, ToolChoice};

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

    // Mock adapter that supports all tool choices
    struct MockAdapter;

    #[async_trait::async_trait]
    impl ProviderAdapter for MockAdapter {
        fn name(&self) -> &'static str {
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
        fn name(&self) -> &'static str {
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
