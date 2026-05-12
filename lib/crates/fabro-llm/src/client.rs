use std::collections::HashMap;
use std::sync::Arc;

use fabro_auth::{ApiCredential, ApiKeyHeader, CredentialSource};
use tracing::debug;

use crate::adapter_registry::{AdapterConfig, factory_for};
use crate::error::Error;
use crate::middleware::{Middleware, NextFn, NextStreamFn};
use crate::provider::{ProviderAdapter, StreamEventStream};
use crate::types::{Request, Response};

/// The core client that routes requests to provider adapters (Section 2.2, 3).
#[derive(Clone)]
pub struct Client {
    providers:        HashMap<String, Arc<dyn ProviderAdapter>>,
    default_provider: Option<String>,
    middleware:       Vec<Arc<dyn Middleware>>,
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

    /// Create a Client from a credential source.
    ///
    /// # Errors
    ///
    /// Returns `Error` if the source cannot resolve credentials or any provider
    /// adapter fails to initialize.
    pub async fn from_source(source: &dyn CredentialSource) -> Result<Self, Error> {
        let resolved = source.resolve().await.map_err(|err| Error::Configuration {
            message: format!("Failed to resolve LLM credentials: {err}"),
            source:  None,
        })?;
        Self::from_credentials(resolved.credentials).await
    }

    /// Create a Client from typed provider credentials.
    ///
    /// # Errors
    ///
    /// Returns `Error` if any provider adapter fails to initialize.
    pub async fn from_credentials(credentials: Vec<ApiCredential>) -> Result<Self, Error> {
        let mut client = Self {
            providers:        HashMap::new(),
            default_provider: None,
            middleware:       Vec::new(),
        };

        for credential in credentials {
            let provider_id = credential.provider.clone();
            let Some(provider) = fabro_model::Catalog::builtin().provider(&provider_id) else {
                return Err(Error::Configuration {
                    message: format!(
                        "Provider \"{provider_id}\" is not supported by credential-only registration"
                    ),
                    source:  None,
                });
            };
            let Some(factory) = factory_for(&provider.adapter) else {
                return Err(Error::Configuration {
                    message: format!(
                        "Provider \"{provider_id}\" uses unsupported adapter \"{}\"",
                        provider.adapter
                    ),
                    source:  None,
                });
            };

            let adapter = factory(AdapterConfig {
                provider_id:   provider.id.to_string(),
                auth_header:   credential.auth_header,
                base_url:      credential.base_url.or_else(|| provider.base_url.clone()),
                extra_headers: credential.extra_headers,
                codex_mode:    credential.codex_mode,
                org_id:        credential.org_id,
                project_id:    credential.project_id,
            });
            client.register_provider(adapter).await?;
        }

        debug!(
            providers = ?client.provider_names(),
            default = ?client.default_provider(),
            "LLM client initialized from typed credentials"
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
                source:  None,
            })?;

        self.providers
            .get(provider_name)
            .cloned()
            .ok_or_else(|| Error::Configuration {
                message: format!("Provider '{provider_name}' not registered"),
                source:  None,
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

    /// Check whether a provider adapter is registered.
    #[must_use]
    pub fn has_provider(&self, name: &str) -> bool {
        self.providers.contains_key(name)
    }

    /// Get the default provider name.
    #[must_use]
    pub fn default_provider(&self) -> Option<&str> {
        self.default_provider.as_deref()
    }
}

pub(crate) fn auth_value(auth_header: &ApiKeyHeader) -> String {
    match auth_header {
        ApiKeyHeader::Bearer(value) | ApiKeyHeader::Custom { value, .. } => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use fabro_auth::{CredentialSource, ResolvedCredentials};
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
                id:            "resp_mock".into(),
                model:         "mock-model".into(),
                provider:      self.provider_name.clone(),
                message:       Message::assistant(&self.response_text),
                finish_reason: FinishReason::Stop,
                usage:         TokenCounts {
                    input_tokens: 10,
                    output_tokens: 20,
                    ..Default::default()
                },
                raw:           None,
                warnings:      vec![],
                rate_limit:    None,
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
            model:            "mock-model".into(),
            messages:         vec![Message::user("Hello")],
            provider:         None,
            tools:            None,
            tool_choice:      None,
            response_format:  None,
            temperature:      None,
            top_p:            None,
            max_tokens:       None,
            stop_sequences:   None,
            reasoning_effort: None,
            speed:            None,
            metadata:         None,
            provider_options: None,
        }
    }

    struct StubSource {
        credentials: Vec<ApiCredential>,
    }

    #[async_trait]
    impl CredentialSource for StubSource {
        async fn resolve(&self) -> anyhow::Result<ResolvedCredentials> {
            Ok(ResolvedCredentials {
                credentials: self.credentials.clone(),
                auth_issues: Vec::new(),
            })
        }

        async fn configured_providers(&self) -> Vec<fabro_model::ProviderId> {
            self.credentials
                .iter()
                .map(|credential| credential.provider.clone())
                .collect()
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
    async fn from_credentials_registers_multiple_providers() {
        let client = Client::from_credentials(vec![
            ApiCredential {
                provider:      fabro_model::Provider::Anthropic.id(),
                auth_header:   ApiKeyHeader::Custom {
                    name:  "x-api-key".to_string(),
                    value: "anthropic-key".to_string(),
                },
                extra_headers: HashMap::new(),
                base_url:      None,
                codex_mode:    false,
                org_id:        None,
                project_id:    None,
            },
            ApiCredential {
                provider:      fabro_model::Provider::OpenAi.id(),
                auth_header:   ApiKeyHeader::Bearer("openai-key".to_string()),
                extra_headers: HashMap::new(),
                base_url:      None,
                codex_mode:    false,
                org_id:        None,
                project_id:    None,
            },
        ])
        .await
        .unwrap();

        let mut providers = client.provider_names();
        providers.sort_unstable();
        assert_eq!(providers, vec!["anthropic", "openai"]);
        assert_eq!(client.default_provider(), Some("anthropic"));
    }

    #[tokio::test]
    async fn from_credentials_supports_openai_compatible_provider_constants() {
        let client = Client::from_credentials(vec![ApiCredential {
            provider:      fabro_model::Provider::Kimi.id(),
            auth_header:   ApiKeyHeader::Bearer("kimi-key".to_string()),
            extra_headers: HashMap::new(),
            base_url:      None,
            codex_mode:    false,
            org_id:        None,
            project_id:    None,
        }])
        .await
        .unwrap();

        assert_eq!(client.provider_names(), vec!["kimi"]);
        assert_eq!(client.default_provider(), Some("kimi"));
    }

    #[tokio::test]
    async fn from_credentials_rejects_custom_provider_id_without_adapter() {
        let result = Client::from_credentials(vec![ApiCredential {
            provider:      fabro_model::ProviderId::new("venice"),
            auth_header:   ApiKeyHeader::Bearer("venice-key".to_string()),
            extra_headers: HashMap::new(),
            base_url:      None,
            codex_mode:    false,
            org_id:        None,
            project_id:    None,
        }])
        .await;
        let Err(err) = result else {
            panic!("custom provider credentials should fail without a registered adapter");
        };

        assert!(matches!(
            err,
            Error::Configuration {
                ref message,
                ..
            } if message == "Provider \"venice\" is not supported by credential-only registration"
        ));
    }

    #[tokio::test]
    async fn from_source_registers_provider_from_resolved_credentials() {
        let source = StubSource {
            credentials: vec![ApiCredential {
                provider:      fabro_model::Provider::Anthropic.id(),
                auth_header:   ApiKeyHeader::Custom {
                    name:  "x-api-key".to_string(),
                    value: "anthropic-key".to_string(),
                },
                extra_headers: HashMap::new(),
                base_url:      None,
                codex_mode:    false,
                org_id:        None,
                project_id:    None,
            }],
        };

        let client = Client::from_source(&source).await.unwrap();

        assert_eq!(client.provider_names(), vec!["anthropic"]);
    }

    #[tokio::test]
    async fn from_source_supports_empty_credentials() {
        let source = StubSource {
            credentials: Vec::new(),
        };

        let client = Client::from_source(&source).await.unwrap();

        assert!(client.provider_names().is_empty());
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
