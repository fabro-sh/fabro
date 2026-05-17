//! Adapter factory registry keyed by [`fabro_model::AdapterKind`].
//!
//! Every adapter kind ships with a matching factory in this module. Tests in
//! this file enforce that the registry covers every adapter kind.
//!
//! Factories take a pre-built [`AdapterConfig`] derived from resolved
//! credentials + provider settings, and produce a boxed
//! [`ProviderAdapter`] ready to register with the [`crate::Client`].
use std::collections::HashMap;
use std::sync::Arc;

use fabro_auth::ApiKeyHeader;
use fabro_model::{AdapterKind, Catalog};

use crate::error::Error;
use crate::provider::ProviderAdapter;
use crate::providers;

/// Configuration passed to an adapter factory. All values are pre-resolved
/// from settings + credentials; factories never touch the environment or the
/// vault directly.
#[derive(Debug, Clone)]
pub struct AdapterConfig {
    /// Provider ID this adapter will register under (used as the registry
    /// name on the resulting adapter).
    pub provider_id:   String,
    /// Authentication header constructed by `fabro-auth` from the provider's
    /// catalog auth policy and resolved credential.
    pub auth_header:   Option<ApiKeyHeader>,
    /// Provider base URL. Native adapters can use their direct-constructor
    /// defaults when this is `None`; OpenAI-compatible providers require it.
    pub base_url:      Option<String>,
    /// Extra HTTP headers attached to every outgoing request.
    pub extra_headers: HashMap<String, String>,
    /// OpenAI-only: route through the ChatGPT Codex backend.
    pub codex_mode:    bool,
    /// OpenAI-only: organization ID.
    pub org_id:        Option<String>,
    /// OpenAI-only: project ID.
    pub project_id:    Option<String>,
    pub catalog:       Option<Arc<Catalog>>,
}

impl AdapterConfig {
    /// Construct a minimal config with just provider ID and auth header.
    pub fn new(provider_id: impl Into<String>, auth_header: ApiKeyHeader) -> Self {
        Self {
            provider_id:   provider_id.into(),
            auth_header:   Some(auth_header),
            base_url:      None,
            extra_headers: HashMap::new(),
            codex_mode:    false,
            org_id:        None,
            project_id:    None,
            catalog:       None,
        }
    }
}

/// Factory function signature. Takes a fully-resolved [`AdapterConfig`] and
/// returns a registered-ready [`ProviderAdapter`].
///
/// Adapter constructors validate provider-specific construction requirements
/// before a provider is registered with the client.
pub type AdapterFactory = fn(AdapterConfig) -> Result<Arc<dyn ProviderAdapter>, Error>;

fn apply_primary_auth_header(
    auth_header: Option<ApiKeyHeader>,
    extra_headers: &mut HashMap<String, String>,
) -> Option<String> {
    match auth_header {
        Some(ApiKeyHeader::Bearer(value)) => Some(value),
        Some(ApiKeyHeader::Custom { name, value }) => {
            extra_headers.insert(name, value);
            None
        }
        None => None,
    }
}

fn build_anthropic_adapter(mut config: AdapterConfig) -> providers::AnthropicAdapter {
    let api_key = apply_primary_auth_header(config.auth_header.take(), &mut config.extra_headers);
    let mut adapter = providers::AnthropicAdapter::new_optional_auth(api_key)
        .with_name(config.provider_id.clone());
    if let Some(base_url) = config.base_url {
        adapter = adapter.with_base_url(base_url);
    }
    if !config.extra_headers.is_empty() {
        adapter = adapter.with_default_headers(config.extra_headers);
    }
    if let Some(catalog) = config.catalog {
        adapter = adapter.with_catalog(catalog);
    }
    adapter
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "Adapter factories share a fallible signature; openai_compatible validates base_url."
)]
fn build_anthropic(config: AdapterConfig) -> Result<Arc<dyn ProviderAdapter>, Error> {
    Ok(Arc::new(build_anthropic_adapter(config)))
}

fn build_openai_adapter(mut config: AdapterConfig) -> providers::OpenAiAdapter {
    let api_key = apply_primary_auth_header(config.auth_header.take(), &mut config.extra_headers);
    let mut adapter =
        providers::OpenAiAdapter::new_optional_auth(api_key).with_name(config.provider_id.clone());
    if let Some(base_url) = config.base_url {
        adapter = adapter.with_base_url(base_url);
    }
    if !config.extra_headers.is_empty() {
        adapter = adapter.with_default_headers(config.extra_headers);
    }
    if config.codex_mode {
        adapter = adapter.with_codex_mode();
    }
    if let Some(org_id) = config.org_id {
        adapter = adapter.with_org_id(org_id);
    }
    if let Some(project_id) = config.project_id {
        adapter = adapter.with_project_id(project_id);
    }
    if let Some(catalog) = config.catalog {
        adapter = adapter.with_catalog(catalog);
    }
    adapter
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "Adapter factories share a fallible signature; openai_compatible validates base_url."
)]
fn build_openai(config: AdapterConfig) -> Result<Arc<dyn ProviderAdapter>, Error> {
    Ok(Arc::new(build_openai_adapter(config)))
}

fn build_gemini_adapter(mut config: AdapterConfig) -> providers::GeminiAdapter {
    let api_key = apply_primary_auth_header(config.auth_header.take(), &mut config.extra_headers);
    let mut adapter =
        providers::GeminiAdapter::new_optional_auth(api_key).with_name(config.provider_id.clone());
    if let Some(base_url) = config.base_url {
        adapter = adapter.with_base_url(base_url);
    }
    if !config.extra_headers.is_empty() {
        adapter = adapter.with_default_headers(config.extra_headers);
    }
    if let Some(catalog) = config.catalog {
        adapter = adapter.with_catalog(catalog);
    }
    adapter
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "Adapter factories share a fallible signature; openai_compatible validates base_url."
)]
fn build_gemini(config: AdapterConfig) -> Result<Arc<dyn ProviderAdapter>, Error> {
    Ok(Arc::new(build_gemini_adapter(config)))
}

fn build_openai_compatible_adapter(
    mut config: AdapterConfig,
) -> Result<providers::OpenAiCompatibleAdapter, Error> {
    let base_url = config.base_url.ok_or_else(|| Error::Configuration {
        message: format!(
            "provider '{}' uses openai_compatible adapter but does not configure base_url",
            config.provider_id
        ),
        source:  None,
    })?;
    let api_key = apply_primary_auth_header(config.auth_header.take(), &mut config.extra_headers);
    let mut adapter = providers::OpenAiCompatibleAdapter::new_optional_auth(api_key, base_url)
        .with_name(config.provider_id);
    if !config.extra_headers.is_empty() {
        adapter = adapter.with_default_headers(config.extra_headers);
    }
    if let Some(catalog) = config.catalog {
        adapter = adapter.with_catalog(catalog);
    }
    Ok(adapter)
}

fn build_openai_compatible(config: AdapterConfig) -> Result<Arc<dyn ProviderAdapter>, Error> {
    Ok(Arc::new(build_openai_compatible_adapter(config)?))
}

/// Return the factory for a known adapter kind.
#[must_use]
pub fn factory_for(adapter_kind: AdapterKind) -> AdapterFactory {
    match adapter_kind {
        AdapterKind::Anthropic => build_anthropic,
        AdapterKind::OpenAi => build_openai,
        AdapterKind::Gemini => build_gemini,
        AdapterKind::OpenAiCompatible => build_openai_compatible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_factory_builds_anthropic_adapter() {
        let config = AdapterConfig::new("anthropic", ApiKeyHeader::Custom {
            name:  "x-api-key".to_string(),
            value: "test-key".to_string(),
        });
        let adapter = factory_for(AdapterKind::Anthropic)(config).unwrap();
        assert_eq!(adapter.name(), "anthropic");
    }

    #[test]
    fn custom_primary_auth_header_is_preserved() {
        let config = AdapterConfig::new("anthropic", ApiKeyHeader::Custom {
            name:  "x-api-key".to_string(),
            value: "test-key".to_string(),
        });

        let adapter = build_anthropic_adapter(config);

        assert!(adapter.http.api_key.is_none());
        assert_eq!(
            adapter.http.default_headers.get("x-api-key"),
            Some(&"test-key".to_string())
        );
    }

    #[test]
    fn custom_primary_auth_header_overrides_extra_header() {
        let config = AdapterConfig {
            provider_id:   "custom".to_string(),
            auth_header:   Some(ApiKeyHeader::Custom {
                name:  "x-api-key".to_string(),
                value: "primary-key".to_string(),
            }),
            base_url:      Some("https://api.custom.test/v1".to_string()),
            extra_headers: HashMap::from([("x-api-key".to_string(), "secondary-key".to_string())]),
            codex_mode:    false,
            org_id:        None,
            project_id:    None,
            catalog:       None,
        };

        let adapter = build_openai_compatible_adapter(config).unwrap();

        assert!(adapter.http.api_key.is_none());
        assert_eq!(
            adapter.http.default_headers.get("x-api-key"),
            Some(&"primary-key".to_string())
        );
    }

    #[test]
    fn openai_compatible_factory_uses_provider_id_for_name() {
        let config = AdapterConfig {
            provider_id:   "kimi".to_string(),
            auth_header:   Some(ApiKeyHeader::Bearer("k".to_string())),
            base_url:      Some("https://api.moonshot.ai/v1".to_string()),
            extra_headers: HashMap::new(),
            codex_mode:    false,
            org_id:        None,
            project_id:    None,
            catalog:       None,
        };
        let adapter = factory_for(AdapterKind::OpenAiCompatible)(config).unwrap();
        assert_eq!(adapter.name(), "kimi");
    }

    #[test]
    fn openai_compatible_factory_preserves_extra_headers() {
        let config = AdapterConfig {
            provider_id:   "portkey".to_string(),
            auth_header:   Some(ApiKeyHeader::Bearer("unused-primary-key".to_string())),
            base_url:      Some("https://api.portkey.ai/v1".to_string()),
            extra_headers: HashMap::from([
                (
                    "x-portkey-api-key".to_string(),
                    "resolved-portkey-key".to_string(),
                ),
                (
                    "x-portkey-provider".to_string(),
                    "@bedrock-prod".to_string(),
                ),
            ]),
            codex_mode:    false,
            org_id:        None,
            project_id:    None,
            catalog:       None,
        };

        let adapter = build_openai_compatible_adapter(config).unwrap();

        assert_eq!(adapter.name(), "portkey");
        assert_eq!(
            adapter.http.default_headers.get("x-portkey-api-key"),
            Some(&"resolved-portkey-key".to_string()),
        );
        assert_eq!(
            adapter.http.default_headers.get("x-portkey-provider"),
            Some(&"@bedrock-prod".to_string()),
        );
    }

    #[test]
    fn anthropic_factory_preserves_extra_headers() {
        let config = AdapterConfig {
            provider_id:   "anthropic-through-portkey".to_string(),
            auth_header:   Some(ApiKeyHeader::Custom {
                name:  "x-api-key".to_string(),
                value: "unused-primary-key".to_string(),
            }),
            base_url:      Some("https://api.portkey.ai/v1".to_string()),
            extra_headers: HashMap::from([(
                "x-portkey-api-key".to_string(),
                "resolved-portkey-key".to_string(),
            )]),
            codex_mode:    false,
            org_id:        None,
            project_id:    None,
            catalog:       None,
        };

        let adapter = build_anthropic_adapter(config);

        assert_eq!(adapter.name(), "anthropic-through-portkey");
        assert_eq!(
            adapter.http.default_headers.get("x-portkey-api-key"),
            Some(&"resolved-portkey-key".to_string()),
        );
    }

    #[test]
    fn openai_compatible_factory_errors_without_base_url() {
        let config = AdapterConfig::new("kimi", ApiKeyHeader::Bearer("k".to_string()));
        let Err(err) = factory_for(AdapterKind::OpenAiCompatible)(config) else {
            panic!("expected missing base_url error");
        };
        assert!(
            err.to_string()
                .contains("uses openai_compatible adapter but does not configure base_url")
        );
    }
}
