use crate::error::SdkError;
use crate::types::{Request, Response, StreamEvent, ToolChoice};
use futures::Stream;
use std::pin::Pin;

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
