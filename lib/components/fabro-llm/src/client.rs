//! Client construction from Fabro configuration and credentials.

use std::sync::Arc;
use std::time::Duration;

use lithos_llm::adapter::ProviderAdapter;
use lithos_llm::catalog::{Catalog, ProviderId};
use lithos_llm::client::{Client, ClientBuildError, ClientBuilder, ProviderBuildIssue};
use lithos_llm::credentials::{CredentialError, CredentialProvider};
use lithos_llm::middleware::{
    Call, InlineLocalFiles, Middleware, Observer, RetryEvent, RetryMiddleware, RetryPolicy,
    RetryStage,
};
use lithos_llm::types::ErrorData;

/// The application name lithos reports to providers that ask, such as the
/// `originator` header on the OpenAI Codex deployment.
const APPLICATION_NAME: &str = "fabro";

/// Default same-provider retry policy applied before visible output.
///
/// Three attempts with short exponential backoff, capped at five seconds.
/// fabro-agent replays after visible output with the same policy.
pub fn default_retry_policy() -> RetryPolicy {
    RetryPolicy::exponential()
        .max_attempts(3)
        .initial_delay(Duration::from_millis(500))
        .max_delay(Duration::from_secs(5))
        .jitter(true)
}

/// One retry the lithos retry middleware decided on.
#[derive(Clone, Debug)]
pub struct RetryNotice {
    /// The failure that ended the attempt.
    pub error:   ErrorData,
    /// The attempt that failed, counted from 1.
    pub attempt: u32,
    /// How long the middleware waits before the next attempt.
    pub delay:   Duration,
    /// Whether the request or its stream failed.
    pub stage:   RetryStage,
}

/// A per-call hook that receives the retries the client performs.
///
/// The retry middleware is shared by every call through a client, so a
/// caller that turns retries into its own events — the agent's durable
/// `LlmRetry` event — inserts a listener into the call's context extensions
/// before dispatch. Calls without a listener are retried silently.
#[derive(Clone)]
pub struct RetryListener(Arc<dyn Fn(RetryNotice) + Send + Sync>);

impl RetryListener {
    pub fn new(listener: impl Fn(RetryNotice) + Send + Sync + 'static) -> Self {
        Self(Arc::new(listener))
    }

    fn notify(&self, notice: RetryNotice) {
        (self.0)(notice);
    }
}

/// Forwards middleware retries to the call's [`RetryListener`], if any.
struct RetryNotifier;

impl Observer for RetryNotifier {
    fn on_retry(&self, call: &Call, retry: RetryEvent<'_>) {
        if let Some(listener) = call.context().extensions().get::<RetryListener>() {
            listener.notify(RetryNotice {
                error:   ErrorData::from(retry.error),
                attempt: retry.attempt,
                delay:   retry.delay,
                stage:   retry.stage,
            });
        }
    }
}

/// The retry middleware Fabro installs: `policy`, reporting each retry to
/// the call's [`RetryListener`].
pub fn retry_middleware(policy: RetryPolicy) -> RetryMiddleware {
    RetryMiddleware::new(policy).observer(RetryNotifier)
}

/// Options for [`build_client`] and [`build_offline_client`].
#[derive(Default)]
pub struct ClientOptions {
    /// Retry policy for the lithos retry middleware. `None` disables retries.
    pub retry:              Option<RetryPolicy>,
    /// Extra middleware, run after retry and attachment inlining.
    pub middleware:         Vec<Arc<dyn Middleware>>,
    /// Custom adapters that replace the built-in adapter for a provider.
    pub adapters:           Vec<(ProviderId, Arc<dyn ProviderAdapter>)>,
    /// HTTP client for provider requests. `None` builds lithos's default.
    pub http:               Option<fabro_http::HttpClient>,
    /// Whether to inline local file attachments. Off for gateway clients that
    /// forward requests to a Fabro server, which inlines them itself.
    pub inline_attachments: bool,
}

impl ClientOptions {
    /// Retries and attachment inlining on, nothing else.
    #[must_use]
    pub fn standard() -> Self {
        Self {
            retry: Some(default_retry_policy()),
            inline_attachments: true,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_retry(mut self, policy: Option<RetryPolicy>) -> Self {
        self.retry = policy;
        self
    }

    #[must_use]
    pub fn with_middleware(mut self, middleware: Arc<dyn Middleware>) -> Self {
        self.middleware.push(middleware);
        self
    }

    #[must_use]
    pub fn with_adapter(mut self, provider: ProviderId, adapter: Arc<dyn ProviderAdapter>) -> Self {
        self.adapters.push((provider, adapter));
        self
    }

    fn adapter_providers(&self) -> impl Iterator<Item = &ProviderId> {
        self.adapters.iter().map(|(provider, _)| provider)
    }

    fn apply(self, mut builder: ClientBuilder) -> ClientBuilder {
        if let Some(http) = self.http {
            builder = builder.http(http);
        }
        if let Some(policy) = self.retry {
            builder = builder.middleware(retry_middleware(policy));
        }
        if self.inline_attachments {
            builder = builder.middleware(InlineLocalFiles::new());
        }
        for middleware in self.middleware {
            builder = builder.middleware_arc(middleware);
        }
        for (provider, adapter) in self.adapters {
            builder = builder.adapter_arc(provider, adapter);
        }
        builder
    }
}

/// A built client plus what the build learned about provider readiness.
pub struct FabroClient {
    pub client:       Client,
    /// Enabled providers with working credentials, in catalog order.
    pub ready:        Vec<ProviderId>,
    /// Enabled providers whose credential material could not be used. The
    /// error's `Display` is the operator-facing line.
    pub auth_issues:  Vec<(ProviderId, CredentialError)>,
    /// Ready providers lithos could not build an adapter for.
    pub build_issues: Vec<ProviderBuildIssue>,
}

impl FabroClient {
    /// Whether `provider` can serve requests through this client.
    #[must_use]
    pub fn has_provider(&self, provider: &ProviderId) -> bool {
        self.client.available_providers().contains(provider)
    }

    /// The providers this client can route to.
    #[must_use]
    pub fn provider_ids(&self) -> Vec<ProviderId> {
        self.client.available_providers().iter().cloned().collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LlmSetupError {
    #[error("failed to build the LLM client")]
    Build(#[from] ClientBuildError),
}

/// Builds a client whose ready providers are those `credentials` can serve.
/// Credentials are re-read on every provider attempt, so a refreshed OAuth
/// token is picked up by the next retry.
pub async fn build_client(
    catalog: Catalog,
    credentials: Arc<dyn CredentialProvider>,
    options: ClientOptions,
) -> Result<FabroClient, LlmSetupError> {
    let builder = Client::builder()
        .catalog(catalog)
        .application(APPLICATION_NAME)
        .credentials_arc(credentials);
    let build = options.apply(builder).build_ready().await?;
    Ok(FabroClient {
        client:       build.client,
        ready:        build.ready,
        auth_issues:  build.credential_issues,
        build_issues: build.issues,
    })
}

/// The enabled providers `credentials` holds material for, in catalog order,
/// without refreshing anything. Cheap enough for listings.
pub async fn configured_providers(
    catalog: &Catalog,
    credentials: &dyn CredentialProvider,
) -> Vec<ProviderId> {
    let mut configured = Vec::new();
    for provider in catalog.providers().filter(|provider| provider.is_enabled()) {
        if credentials.is_configured(provider).await {
            configured.push(provider.id().clone());
        }
    }
    configured
}

/// Builds a client that needs no credentials: every available provider is
/// served by a custom adapter from `options.adapters`, such as the
/// `fabro exec` gateway or a test double.
pub fn build_offline_client(
    catalog: Catalog,
    options: ClientOptions,
) -> Result<FabroClient, LlmSetupError> {
    let ready: Vec<ProviderId> = options.adapter_providers().cloned().collect();
    let builder = Client::builder()
        .catalog(catalog)
        .application(APPLICATION_NAME)
        .enabled_providers(ready.iter().cloned());
    let build = options.apply(builder).build()?;
    Ok(FabroClient {
        client: build.client,
        ready,
        auth_issues: Vec::new(),
        build_issues: build.issues,
    })
}

#[cfg(test)]
mod tests {
    use fabro_auth::test_support::env_credential_source;

    use super::*;
    use crate::test_support::test_catalog;

    #[tokio::test]
    async fn ready_providers_follow_credentials_and_policy() {
        let source = env_credential_source(|name| match name {
            "OPENAI_API_KEY" | "BEDROCK_API_KEY" => Some("key".to_string()),
            _ => None,
        });
        let built = build_client(test_catalog(), source, ClientOptions::standard())
            .await
            .unwrap();
        assert!(built.has_provider(&ProviderId::new("openai")));
        assert!(
            !built.has_provider(&ProviderId::new("bedrock")),
            "disabled providers never become ready"
        );
        assert!(!built.has_provider(&ProviderId::new("anthropic")));
        assert!(built.auth_issues.is_empty());
        assert!(built.build_issues.is_empty(), "{:?}", built.build_issues);
    }
}
