use std::collections::HashMap;
use std::sync::Arc;

use fabro_auth::{ApiCredential, ApiKeyHeader, CredentialSource};
use fabro_model::catalog::LlmCatalogSettings;
use fabro_model::{Catalog, ProviderId};
use tracing::debug;

use crate::adapter_registry::{AdapterConfig, factory_for};
use crate::error::Error;
use crate::middleware::{Middleware, NextFn, NextStreamFn};
use crate::provider::{ProviderAdapter, StreamEventStream};
use crate::types::{Request, Response, Speed};

/// The core client that routes requests to provider adapters (Section 2.2, 3).
#[derive(Clone)]
pub struct Client {
    providers:        HashMap<String, Arc<dyn ProviderAdapter>>,
    default_provider: Option<String>,
    middleware:       Vec<Arc<dyn Middleware>>,
    catalog:          Option<Arc<Catalog>>,
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
            catalog: None,
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

    pub async fn from_source_with_catalog(
        source: &dyn CredentialSource,
        catalog: Arc<Catalog>,
    ) -> Result<Self, Error> {
        let resolved =
            source
                .resolve_for_catalog(&catalog)
                .await
                .map_err(|err| Error::Configuration {
                    message: format!("Failed to resolve LLM credentials: {err}"),
                    source:  None,
                })?;
        Self::from_credentials_with_catalog(resolved.credentials, catalog).await
    }

    /// Create a Client from typed provider credentials.
    ///
    /// # Errors
    ///
    /// Returns `Error` if any provider adapter fails to initialize.
    pub async fn from_credentials(credentials: Vec<ApiCredential>) -> Result<Self, Error> {
        let catalog = Arc::new(
            Catalog::from_builtin_with_overrides(&LlmCatalogSettings::default()).map_err(
                |err| Error::Configuration {
                    message: "Failed to build bootstrap LLM catalog".to_string(),
                    source:  Some(Arc::new(err)),
                },
            )?,
        );
        Self::from_credentials_with_catalog(credentials, catalog).await
    }

    pub async fn from_credentials_with_catalog(
        credentials: Vec<ApiCredential>,
        catalog: Arc<Catalog>,
    ) -> Result<Self, Error> {
        let mut client = Self {
            providers:        HashMap::new(),
            default_provider: None,
            middleware:       Vec::new(),
            catalog:          Some(Arc::clone(&catalog)),
        };

        for credential in credentials {
            let provider_id = credential.provider.clone();
            let Some(provider) = catalog.provider(&provider_id) else {
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
                catalog:       Some(Arc::clone(&catalog)),
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

    fn canonical_provider_name(&self, provider_name: &str) -> String {
        self.catalog
            .as_ref()
            .and_then(|catalog| catalog.provider(&ProviderId::new(provider_name)))
            .map_or_else(
                || provider_name.to_string(),
                |provider| provider.id.to_string(),
            )
    }

    /// Resolve the provider for a request.
    fn resolve_provider(&self, request: &Request) -> Result<Arc<dyn ProviderAdapter>, Error> {
        let catalog_provider = self.catalog.as_ref().and_then(|catalog| {
            catalog
                .get(&request.model)
                .map(|info| info.provider.to_string())
        });

        let provider_name = request
            .provider
            .as_deref()
            .or(catalog_provider.as_deref())
            .or(self.default_provider.as_deref())
            .ok_or_else(|| Error::Configuration {
                message: "No provider specified and no default provider set".into(),
                source:  None,
            })?;
        let provider_name = self.canonical_provider_name(provider_name);

        self.providers
            .get(&provider_name)
            .cloned()
            .ok_or_else(|| Error::Configuration {
                message: format!("Provider '{provider_name}' not registered"),
                source:  None,
            })
    }

    fn validate_request_controls(&self, request: &Request) -> Result<(), Error> {
        let Some(catalog) = &self.catalog else {
            return Ok(());
        };
        let Some(settings) = catalog.model_settings(&request.model) else {
            return Ok(());
        };
        let model_id = catalog
            .get(&request.model)
            .map_or(request.model.as_str(), |model| model.id.as_str());

        if let Some(effort) = request.reasoning_effort {
            if !settings.controls.reasoning_effort.contains(&effort) {
                return Err(Error::Configuration {
                    message: format!(
                        "model '{model_id}' does not support reasoning_effort '{effort}'; allowed values: {}",
                        format_control_values(&settings.controls.reasoning_effort),
                    ),
                    source:  None,
                });
            }
        }

        if let Some(speed) = request.speed {
            if speed != Speed::Standard && !settings.controls.speed.contains(&speed) {
                return Err(Error::Configuration {
                    message: format!(
                        "model '{model_id}' does not support speed '{speed}'; allowed values: standard{}",
                        format_additional_speeds(&settings.controls.speed),
                    ),
                    source:  None,
                });
            }
        }

        Ok(())
    }

    /// Send a blocking request (Section 4.1).
    ///
    /// # Errors
    ///
    /// Returns `Error::Configuration` if no provider is specified or
    /// registered, or any provider/middleware error encountered during the
    /// request.
    pub async fn complete(&self, request: &Request) -> Result<Response, Error> {
        self.validate_request_controls(request)?;
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
        self.validate_request_controls(request)?;
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
            || self
                .catalog
                .as_ref()
                .and_then(|catalog| catalog.provider(&ProviderId::new(name)))
                .is_some_and(|provider| self.providers.contains_key(provider.id.as_str()))
    }

    /// Get the default provider name.
    #[must_use]
    pub fn default_provider(&self) -> Option<&str> {
        self.default_provider.as_deref()
    }
}

fn format_control_values<T: ToString>(values: &[T]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn format_additional_speeds(values: &[Speed]) -> String {
    if values.is_empty() {
        String::new()
    } else {
        format!(", {}", format_control_values(values))
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
    use fabro_model::catalog::LlmCatalogSettings;
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

    fn catalog_with(overrides: &str) -> Arc<Catalog> {
        let settings: LlmCatalogSettings = toml::from_str(overrides).unwrap();
        Arc::new(Catalog::from_builtin_with_overrides(&settings).unwrap())
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
    async fn complete_rejects_unsupported_reasoning_effort_before_dispatch() {
        let catalog =
            Arc::new(Catalog::from_builtin_with_overrides(&LlmCatalogSettings::default()).unwrap());
        let mut client = Client::new(HashMap::new(), None, vec![]);
        client.catalog = Some(Arc::clone(&catalog));
        client
            .register_provider(Arc::new(MockProvider::new("kimi", "should not dispatch")))
            .await
            .unwrap();

        let mut request = test_request();
        request.model = "kimi-k2.5".to_string();
        request.provider = Some("kimi".to_string());
        request.reasoning_effort = Some(ReasoningEffort::High);

        let err = client.complete(&request).await.unwrap_err();

        assert!(matches!(
            err,
            Error::Configuration {
                ref message,
                ..
            } if message.contains("model 'kimi-k2.5' does not support reasoning_effort 'high'")
        ));
    }

    #[tokio::test]
    async fn complete_rejects_unsupported_speed_before_dispatch() {
        let catalog =
            Arc::new(Catalog::from_builtin_with_overrides(&LlmCatalogSettings::default()).unwrap());
        let mut client = Client::new(HashMap::new(), None, vec![]);
        client.catalog = Some(Arc::clone(&catalog));
        client
            .register_provider(Arc::new(MockProvider::new("openai", "should not dispatch")))
            .await
            .unwrap();

        let mut request = test_request();
        request.model = "gpt-5.4".to_string();
        request.provider = Some("openai".to_string());
        request.speed = Some(Speed::Fast);

        let err = client.complete(&request).await.unwrap_err();

        assert!(matches!(
            err,
            Error::Configuration {
                ref message,
                ..
            } if message.contains("model 'gpt-5.4' does not support speed 'fast'")
        ));
    }

    #[tokio::test]
    async fn complete_accepts_standard_speed_without_catalog_declaration() {
        let catalog =
            Arc::new(Catalog::from_builtin_with_overrides(&LlmCatalogSettings::default()).unwrap());
        let mut client = Client::new(HashMap::new(), None, vec![]);
        client.catalog = Some(Arc::clone(&catalog));
        client
            .register_provider(Arc::new(MockProvider::new("openai", "standard")))
            .await
            .unwrap();

        let mut request = test_request();
        request.model = "gpt-5.4".to_string();
        request.provider = Some("openai".to_string());
        request.speed = Some(Speed::Standard);

        let response = client.complete(&request).await.unwrap();

        assert_eq!(response.text(), "standard");
    }

    #[tokio::test]
    async fn complete_skips_control_validation_for_unknown_model_passthrough() {
        let catalog =
            Arc::new(Catalog::from_builtin_with_overrides(&LlmCatalogSettings::default()).unwrap());
        let mut client = Client::new(HashMap::new(), None, vec![]);
        client.catalog = Some(Arc::clone(&catalog));
        client
            .register_provider(Arc::new(MockProvider::new("openai", "passthrough")))
            .await
            .unwrap();

        let mut request = test_request();
        request.model = "custom-model".to_string();
        request.provider = Some("openai".to_string());
        request.reasoning_effort = Some(ReasoningEffort::High);
        request.speed = Some(Speed::Fast);

        let response = client.complete(&request).await.unwrap();

        assert_eq!(response.text(), "passthrough");
    }

    #[tokio::test]
    async fn stream_rejects_unsupported_speed_before_dispatch() {
        let catalog =
            Arc::new(Catalog::from_builtin_with_overrides(&LlmCatalogSettings::default()).unwrap());
        let mut client = Client::new(HashMap::new(), None, vec![]);
        client.catalog = Some(Arc::clone(&catalog));
        client
            .register_provider(Arc::new(MockProvider::new("openai", "should not dispatch")))
            .await
            .unwrap();

        let mut request = test_request();
        request.model = "gpt-5.4".to_string();
        request.provider = Some("openai".to_string());
        request.speed = Some(Speed::Fast);

        let Err(err) = client.stream(&request).await else {
            panic!("unsupported speed should fail before stream dispatch");
        };

        assert!(matches!(
            err,
            Error::Configuration {
                ref message,
                ..
            } if message.contains("model 'gpt-5.4' does not support speed 'fast'")
        ));
    }

    #[tokio::test]
    async fn from_credentials_registers_multiple_providers() {
        let client = Client::from_credentials(vec![
            ApiCredential {
                provider:      fabro_model::Provider::Anthropic.id(),
                auth_header:   Some(ApiKeyHeader::Custom {
                    name:  "x-api-key".to_string(),
                    value: "anthropic-key".to_string(),
                }),
                extra_headers: HashMap::new(),
                base_url:      None,
                codex_mode:    false,
                org_id:        None,
                project_id:    None,
            },
            ApiCredential {
                provider:      fabro_model::Provider::OpenAi.id(),
                auth_header:   Some(ApiKeyHeader::Bearer("openai-key".to_string())),
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
            auth_header:   Some(ApiKeyHeader::Bearer("kimi-key".to_string())),
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
            auth_header:   Some(ApiKeyHeader::Bearer("venice-key".to_string())),
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
                auth_header:   Some(ApiKeyHeader::Custom {
                    name:  "x-api-key".to_string(),
                    value: "anthropic-key".to_string(),
                }),
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
    async fn from_credentials_with_catalog_registers_custom_openai_compatible_provider() {
        let catalog = catalog_with(
            r#"
[providers.venice]
display_name = "Venice"
adapter = "openai_compatible"
base_url = "https://api.venice.ai/api/v1"
credentials = ["env:VENICE_API_KEY"]
aliases = ["venice-ai"]

[models."venice-large"]
provider = "venice"
display_name = "Venice Large"
family = "venice"
default = true

[models."venice-large".limits]
context_window = 128000

[models."venice-large".features]
tools = true
vision = false
reasoning = false
effort = false
"#,
        );

        let client = Client::from_credentials_with_catalog(
            vec![ApiCredential {
                provider:      fabro_model::ProviderId::new("venice"),
                auth_header:   Some(ApiKeyHeader::Bearer("venice-key".to_string())),
                extra_headers: HashMap::new(),
                base_url:      None,
                codex_mode:    false,
                org_id:        None,
                project_id:    None,
            }],
            Arc::clone(&catalog),
        )
        .await
        .unwrap();

        assert_eq!(client.provider_names(), vec!["venice"]);
        assert!(client.has_provider("venice"));
        assert!(client.has_provider("venice-ai"));
    }

    #[tokio::test]
    async fn resolve_provider_accepts_catalog_provider_alias() {
        let catalog = catalog_with(
            r#"
[providers.venice]
display_name = "Venice"
adapter = "openai_compatible"
base_url = "https://api.venice.ai/api/v1"
credentials = ["env:VENICE_API_KEY"]
aliases = ["venice-ai"]

[models."venice-large"]
provider = "venice"
display_name = "Venice Large"
family = "venice"
default = true

[models."venice-large".limits]
context_window = 128000

[models."venice-large".features]
tools = true
vision = false
reasoning = false
effort = false
"#,
        );

        let client = Client::from_credentials_with_catalog(
            vec![ApiCredential {
                provider:      fabro_model::ProviderId::new("venice"),
                auth_header:   Some(ApiKeyHeader::Bearer("venice-key".to_string())),
                extra_headers: HashMap::new(),
                base_url:      None,
                codex_mode:    false,
                org_id:        None,
                project_id:    None,
            }],
            Arc::clone(&catalog),
        )
        .await
        .unwrap();
        let mut request = test_request();
        request.provider = Some("venice-ai".to_string());

        let provider = client.resolve_provider(&request).unwrap();

        assert_eq!(provider.name(), "venice");
    }

    #[tokio::test]
    async fn from_credentials_with_catalog_registers_header_only_provider() {
        let catalog = catalog_with(
            r#"
[providers.portkey]
display_name = "Portkey Bedrock"
adapter = "anthropic"
base_url = "https://api.portkey.ai/v1"

[providers.portkey.extra_headers]
x-portkey-api-key = { literal = "pk-live" }

[models."portkey-claude"]
provider = "portkey"
display_name = "Portkey Claude"
family = "claude"
default = true

[models."portkey-claude".limits]
context_window = 200000

[models."portkey-claude".features]
tools = true
vision = true
reasoning = true
effort = true
"#,
        );

        let client = Client::from_credentials_with_catalog(
            vec![ApiCredential {
                provider:      fabro_model::ProviderId::new("portkey"),
                auth_header:   None,
                extra_headers: HashMap::from([(
                    "x-portkey-api-key".to_string(),
                    "pk-live".to_string(),
                )]),
                base_url:      None,
                codex_mode:    false,
                org_id:        None,
                project_id:    None,
            }],
            Arc::clone(&catalog),
        )
        .await
        .unwrap();

        assert_eq!(client.provider_names(), vec!["portkey"]);
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
