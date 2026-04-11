use std::collections::HashMap;
use std::sync::Arc;

use tracing::debug;

use crate::error::Error;
use crate::middleware::{Middleware, NextFn, NextStreamFn};
use crate::provider::{ProviderAdapter, StreamEventStream};
use crate::providers;
use crate::types::{Request, Response};

/// The core client that routes requests to provider adapters (Section 2.2, 3).
#[derive(Clone)]
pub struct Client {
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
    default_provider: Option<String>,
    middleware: Vec<Arc<dyn Middleware>>,
}

impl Client {
    /// Create a new Client with explicit configuration.
    #[must_use]
    pub fn new(
        providers: HashMap<String, Arc<dyn ProviderAdapter>>,
        default_provider: Option<String>,
        middleware: Vec<Arc<dyn Middleware>>,
    ) -> Self {
        Self {
            providers,
            default_provider,
            middleware,
        }
    }

    /// Create a Client from environment variables (Section 2.2).
    /// Registers providers whose API keys are present in the environment.
    /// The first registered provider becomes the default.
    ///
    /// # Errors
    ///
    /// Returns `Error` if any provider adapter fails to initialize.
    pub async fn from_env() -> Result<Self, Error> {
        Self::from_lookup(|name| std::env::var(name).ok()).await
    }

    /// Create a Client from a custom variable lookup.
    ///
    /// This is useful when credentials come from a source other than process
    /// environment variables, while still preserving the env-style provider
    /// configuration surface.
    pub async fn from_lookup<F>(lookup: F) -> Result<Self, Error>
    where
        F: Fn(&str) -> Option<String>,
    {
        let mut client = Self {
            providers: HashMap::new(),
            default_provider: None,
            middleware: Vec::new(),
        };

        // Register providers whose API keys are present in the environment.
        // Order determines which becomes the default provider.
        if let Some(key) = lookup("ANTHROPIC_API_KEY") {
            let mut adapter = providers::AnthropicAdapter::new(key);
            if let Some(base_url) = lookup("ANTHROPIC_BASE_URL") {
                adapter = adapter.with_base_url(base_url);
            }
            client.register_provider(Arc::new(adapter)).await?;
        }
        if let Some(key) = lookup("OPENAI_API_KEY") {
            let mut adapter = providers::OpenAiAdapter::new(key);
            if let Some(account_id) = lookup("CHATGPT_ACCOUNT_ID") {
                // Codex OAuth: route through chatgpt.com backend with required headers
                adapter = adapter
                    .with_base_url("https://chatgpt.com/backend-api/codex")
                    .with_codex_mode();
                let mut headers = std::collections::HashMap::new();
                headers.insert("ChatGPT-Account-Id".to_string(), account_id);
                headers.insert("originator".to_string(), "fabro".to_string());
                adapter = adapter.with_default_headers(headers);
            } else if let Some(base_url) = lookup("OPENAI_BASE_URL") {
                adapter = adapter.with_base_url(base_url);
            }
            if let Some(org_id) = lookup("OPENAI_ORG_ID") {
                adapter = adapter.with_org_id(org_id);
            }
            if let Some(project_id) = lookup("OPENAI_PROJECT_ID") {
                adapter = adapter.with_project_id(project_id);
            }
            client.register_provider(Arc::new(adapter)).await?;
        }
        if let Some(key) = lookup("GEMINI_API_KEY").or_else(|| lookup("GOOGLE_API_KEY")) {
            let mut adapter = providers::GeminiAdapter::new(key);
            if let Some(base_url) = lookup("GEMINI_BASE_URL") {
                adapter = adapter.with_base_url(base_url);
            }
            client.register_provider(Arc::new(adapter)).await?;
        }
        if let Some(key) = lookup("KIMI_API_KEY") {
            let adapter =
                providers::OpenAiCompatibleAdapter::new(key, "https://api.moonshot.ai/v1")
                    .with_name("kimi");
            client.register_provider(Arc::new(adapter)).await?;
        }
        if let Some(key) = lookup("ZAI_API_KEY") {
            let adapter =
                providers::OpenAiCompatibleAdapter::new(key, "https://api.z.ai/api/coding/paas/v4")
                    .with_name("zai");
            client.register_provider(Arc::new(adapter)).await?;
        }
        if let Some(key) = lookup("MINIMAX_API_KEY") {
            let adapter = providers::OpenAiCompatibleAdapter::new(key, "https://api.minimax.io/v1")
                .with_name("minimax");
            client.register_provider(Arc::new(adapter)).await?;
        }
        if let Some(key) = lookup("INCEPTION_API_KEY") {
            let adapter =
                providers::OpenAiCompatibleAdapter::new(key, "https://api.inceptionlabs.ai/v1")
                    .with_name("inception");
            client.register_provider(Arc::new(adapter)).await?;
        }

        debug!(
            providers = ?client.provider_names(),
            default = ?client.default_provider(),
            "LLM client initialized from environment"
        );

        Ok(client)
    }

    /// Register a provider adapter. Calls `initialize()` on the adapter
    /// (Section 2.4).
    ///
    /// # Errors
    ///
    /// Returns `Error` if the adapter's `initialize()` method fails.
    pub async fn register_provider(
        &mut self,
        adapter: Arc<dyn ProviderAdapter>,
    ) -> Result<(), Error> {
        adapter.initialize().await?;
        let name = adapter.name().to_string();
        if self.default_provider.is_none() {
            self.default_provider = Some(name.clone());
        }
        self.providers.insert(name.clone(), adapter);
        debug!(provider = %name, "Provider registered");
        Ok(())
    }

    /// Add middleware.
    pub fn add_middleware(&mut self, mw: Arc<dyn Middleware>) {
        self.middleware.push(mw);
    }

    /// Resolve the provider for a request.
    fn resolve_provider(&self, request: &Request) -> Result<Arc<dyn ProviderAdapter>, Error> {
        let catalog_provider = fabro_model::Catalog::builtin()
            .get(&request.model)
            .map(|info| info.provider.to_string());

        let provider_name = request
            .provider
            .as_deref()
            .or(catalog_provider.as_deref())
            .or(self.default_provider.as_deref())
            .ok_or_else(|| Error::Configuration {
                message: "No provider specified and no default provider set".into(),
                source: None,
            })?;

        self.providers
            .get(provider_name)
            .cloned()
            .ok_or_else(|| Error::Configuration {
                message: format!("Provider '{provider_name}' not registered"),
                source: None,
            })
    }

    /// Send a blocking request (Section 4.1).
    ///
    /// # Errors
    ///
    /// Returns `Error::Configuration` if no provider is specified or
    /// registered, or any provider/middleware error encountered during the
    /// request.
    pub async fn complete(&self, request: &Request) -> Result<Response, Error> {
        let provider = self.resolve_provider(request)?;

        if self.middleware.is_empty() {
            return provider.complete(request).await;
        }

        // Build middleware chain
        let provider_clone = provider.clone();
        let base: NextFn = Arc::new(move |req: Request| {
            let p = provider_clone.clone();
            Box::pin(async move { p.complete(&req).await })
        });

        let chain = self.middleware.iter().rev().fold(base, |next, mw| {
            let mw = mw.clone();
            Arc::new(move |req: Request| {
                let mw = mw.clone();
                let next = next.clone();
                Box::pin(async move { mw.handle_complete(req, next).await })
            })
        });

        chain(request.clone()).await
    }

    /// Send a streaming request (Section 4.2).
    ///
    /// # Errors
    ///
    /// Returns `Error::Configuration` if no provider is specified or
    /// registered, or any provider/middleware error encountered during the
    /// request.
    pub async fn stream(&self, request: &Request) -> Result<StreamEventStream, Error> {
        let provider = self.resolve_provider(request)?;

        if self.middleware.is_empty() {
            return provider.stream(request).await;
        }

        // Build streaming middleware chain
        let provider_clone = provider.clone();
        let base: NextStreamFn = Arc::new(move |req: Request| {
            let p = provider_clone.clone();
            Box::pin(async move { p.stream(&req).await })
        });

        let chain = self.middleware.iter().rev().fold(base, |next, mw| {
            let mw = mw.clone();
            Arc::new(move |req: Request| {
                let mw = mw.clone();
                let next = next.clone();
                Box::pin(async move { mw.handle_stream(req, next).await })
            })
        });

        chain(request.clone()).await
    }

    /// Close all provider adapters.
    ///
    /// # Errors
    ///
    /// Returns any error from a provider adapter's `close()` method.
    pub async fn close(&self) -> Result<(), Error> {
        for provider in self.providers.values() {
            provider.close().await?;
        }
        Ok(())
    }

    /// Get the list of registered provider names.
    #[must_use]
    pub fn provider_names(&self) -> Vec<&str> {
        self.providers
            .keys()
            .map(std::string::String::as_str)
            .collect()
    }

    /// Get the default provider name.
    #[must_use]
    pub fn default_provider(&self) -> Option<&str> {
        self.default_provider.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use futures::stream;

    use super::*;
    use crate::types::*;

    /// A mock provider for testing.
    struct MockProvider {
        provider_name: String,
        response_text: String,
    }

    impl MockProvider {
        fn new(name: &str, response: &str) -> Self {
            Self {
                provider_name: name.to_string(),
                response_text: response.to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl ProviderAdapter for MockProvider {
        fn name(&self) -> &str {
            &self.provider_name
        }

        async fn complete(&self, _request: &Request) -> Result<Response, Error> {
            Ok(Response {
                id: "resp_mock".into(),
                model: "mock-model".into(),
                provider: self.provider_name.clone(),
                message: Message::assistant(&self.response_text),
                finish_reason: FinishReason::Stop,
                usage: TokenCounts {
                    input_tokens: 10,
                    output_tokens: 20,
                    ..Default::default()
                },
                raw: None,
                warnings: vec![],
                rate_limit: None,
            })
        }

        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, Error> {
            let text = self.response_text.clone();
            let provider = self.provider_name.clone();
            let events = vec![
                Ok(StreamEvent::text_delta(&text, Some("t1".into()))),
                Ok(StreamEvent::finish(
                    FinishReason::Stop,
                    TokenCounts::default(),
                    Response {
                        id: "resp_mock".into(),
                        model: "mock-model".into(),
                        provider,
                        message: Message::assistant(&text),
                        finish_reason: FinishReason::Stop,
                        usage: TokenCounts::default(),
                        raw: None,
                        warnings: vec![],
                        rate_limit: None,
                    },
                )),
            ];
            Ok(Box::pin(stream::iter(events)))
        }
    }

    fn test_request() -> Request {
        Request {
            model: "mock-model".into(),
            messages: vec![Message::user("Hello")],
            provider: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop_sequences: None,
            reasoning_effort: None,
            speed: None,
            metadata: None,
            provider_options: None,
        }
    }

    #[tokio::test]
    async fn complete_routes_to_default_provider() {
        let mut client = Client::new(HashMap::new(), None, vec![]);
        client
            .register_provider(Arc::new(MockProvider::new("test", "Hello!")))
            .await
            .unwrap();

        let response = client.complete(&test_request()).await.unwrap();
        assert_eq!(response.text(), "Hello!");
        assert_eq!(response.provider, "test");
    }

    #[tokio::test]
    async fn complete_routes_to_named_provider() {
        let mut client = Client::new(HashMap::new(), None, vec![]);
        client
            .register_provider(Arc::new(MockProvider::new("provider_a", "from A")))
            .await
            .unwrap();
        client
            .register_provider(Arc::new(MockProvider::new("provider_b", "from B")))
            .await
            .unwrap();

        let mut req = test_request();
        req.provider = Some("provider_b".into());
        let response = client.complete(&req).await.unwrap();
        assert_eq!(response.text(), "from B");
    }

    #[tokio::test]
    async fn complete_errors_on_missing_provider() {
        let client = Client::new(HashMap::new(), None, vec![]);
        let result = client.complete(&test_request()).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::Configuration { .. }));
    }

    #[tokio::test]
    async fn complete_errors_on_unknown_provider() {
        let mut client = Client::new(HashMap::new(), None, vec![]);
        client
            .register_provider(Arc::new(MockProvider::new("test", "Hello")))
            .await
            .unwrap();

        let mut req = test_request();
        req.provider = Some("nonexistent".into());
        let result = client.complete(&req).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::Configuration { .. }));
    }

    #[tokio::test]
    async fn register_sets_first_as_default() {
        let mut client = Client::new(HashMap::new(), None, vec![]);
        assert_eq!(client.default_provider(), None);

        client
            .register_provider(Arc::new(MockProvider::new("first", "1")))
            .await
            .unwrap();
        assert_eq!(client.default_provider(), Some("first"));

        client
            .register_provider(Arc::new(MockProvider::new("second", "2")))
            .await
            .unwrap();
        assert_eq!(client.default_provider(), Some("first"));
    }

    #[tokio::test]
    async fn stream_routes_to_provider() {
        use futures::StreamExt;

        let mut client = Client::new(HashMap::new(), None, vec![]);
        client
            .register_provider(Arc::new(MockProvider::new("test", "streamed")))
            .await
            .unwrap();

        let mut stream = client.stream(&test_request()).await.unwrap();
        let first = stream.next().await.unwrap().unwrap();
        match &first {
            StreamEvent::TextDelta { delta, .. } => assert_eq!(delta, "streamed"),
            other => panic!("Expected TextDelta, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn provider_names_returns_registered() {
        let mut client = Client::new(HashMap::new(), None, vec![]);
        client
            .register_provider(Arc::new(MockProvider::new("alpha", "")))
            .await
            .unwrap();
        client
            .register_provider(Arc::new(MockProvider::new("beta", "")))
            .await
            .unwrap();
        let mut names = client.provider_names();
        names.sort_unstable();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    /// Test middleware gets called
    struct UppercaseMiddleware;

    #[async_trait::async_trait]
    impl Middleware for UppercaseMiddleware {
        async fn handle_complete(&self, request: Request, next: NextFn) -> Result<Response, Error> {
            let mut response = next(request).await?;
            let text = response.text().to_uppercase();
            response.message = Message::assistant(text);
            Ok(response)
        }

        async fn handle_stream(
            &self,
            request: Request,
            next: NextStreamFn,
        ) -> Result<StreamEventStream, Error> {
            next(request).await
        }
    }

    #[tokio::test]
    async fn middleware_wraps_complete() {
        let mut client = Client::new(HashMap::new(), None, vec![]);
        client
            .register_provider(Arc::new(MockProvider::new("test", "hello")))
            .await
            .unwrap();
        client.add_middleware(Arc::new(UppercaseMiddleware));

        let response = client.complete(&test_request()).await.unwrap();
        assert_eq!(response.text(), "HELLO");
    }
}
