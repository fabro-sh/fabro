//! Adapter factory registry keyed by stable adapter strings.
//!
//! Mirrors the static [`fabro_model::adapter`] metadata: every metadata key
//! ships with a matching factory in this module. Tests in this file enforce
//! that the registry covers every metadata key and never adds keys that have
//! no metadata.
//!
//! Factories take a pre-built [`AdapterConfig`] derived from resolved
//! credentials + provider settings, and produce a boxed
//! [`ProviderAdapter`] ready to register with the [`crate::Client`].
//!
//! This is the seam the rest of the workspace will eventually use to retire
//! the per-`Provider`-variant match in [`crate::Client::from_credentials`].

use std::collections::HashMap;
use std::sync::Arc;

use fabro_auth::ApiKeyHeader;
use fabro_model::Catalog;
use fabro_model::adapter::{self as model_adapter, AdapterMetadata};

use crate::client::auth_value;
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
    /// Authentication header constructed by `fabro-auth` from the resolved
    /// credential and the adapter's [`fabro_model::ApiKeyHeaderPolicy`].
    pub auth_header:   Option<ApiKeyHeader>,
    /// Provider base URL override. `None` means use the adapter's built-in
    /// default.
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
/// Adapter constructors are infallible today; if a future adapter needs to
/// fail at construction time, add a separate fallible factory variant
/// rather than re-shaping every existing factory.
pub type AdapterFactory = fn(AdapterConfig) -> Arc<dyn ProviderAdapter>;

fn auth_value_optional(auth_header: Option<&ApiKeyHeader>) -> Option<String> {
    auth_header.map(auth_value)
}

fn build_anthropic_adapter(config: AdapterConfig) -> providers::AnthropicAdapter {
    let mut adapter = providers::AnthropicAdapter::new_optional_auth(auth_value_optional(
        config.auth_header.as_ref(),
    ))
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

fn build_anthropic(config: AdapterConfig) -> Arc<dyn ProviderAdapter> {
    Arc::new(build_anthropic_adapter(config))
}

fn build_openai_adapter(config: AdapterConfig) -> providers::OpenAiAdapter {
    let mut adapter = providers::OpenAiAdapter::new_optional_auth(auth_value_optional(
        config.auth_header.as_ref(),
    ))
    .with_name(config.provider_id.clone());
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

fn build_openai(config: AdapterConfig) -> Arc<dyn ProviderAdapter> {
    Arc::new(build_openai_adapter(config))
}

fn build_gemini_adapter(config: AdapterConfig) -> providers::GeminiAdapter {
    let mut adapter = providers::GeminiAdapter::new_optional_auth(auth_value_optional(
        config.auth_header.as_ref(),
    ))
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

fn build_gemini(config: AdapterConfig) -> Arc<dyn ProviderAdapter> {
    Arc::new(build_gemini_adapter(config))
}

fn build_openai_compatible_adapter(config: AdapterConfig) -> providers::OpenAiCompatibleAdapter {
    // `openai_compatible` providers vary widely in base URL; the catalog must
    // pre-resolve `[llm.providers.<id>].base_url` before constructing
    // `AdapterConfig`. There is no sensible default — silently routing to one
    // provider's host would produce wrong-host requests for every other.
    let base_url = config.base_url.expect(
        "openai_compatible adapter requires a base_url; resolve it from provider settings before \
         building AdapterConfig",
    );
    let mut adapter = providers::OpenAiCompatibleAdapter::new_optional_auth(
        auth_value_optional(config.auth_header.as_ref()),
        base_url,
    )
    .with_name(config.provider_id);
    if !config.extra_headers.is_empty() {
        adapter = adapter.with_default_headers(config.extra_headers);
    }
    if let Some(catalog) = config.catalog {
        adapter = adapter.with_catalog(catalog);
    }
    adapter
}

fn build_openai_compatible(config: AdapterConfig) -> Arc<dyn ProviderAdapter> {
    Arc::new(build_openai_compatible_adapter(config))
}

/// Single source of truth pairing every adapter key with its factory. Both
/// `factory_for` and `registered_keys` derive from this table.
const FACTORIES: &[(&str, AdapterFactory)] = &[
    (model_adapter::ANTHROPIC.key, build_anthropic),
    (model_adapter::OPENAI.key, build_openai),
    (model_adapter::GEMINI.key, build_gemini),
    (
        model_adapter::OPENAI_COMPATIBLE.key,
        build_openai_compatible,
    ),
];

/// Look up a factory by adapter key. Returns `None` if the key has no factory
/// registered.
#[must_use]
pub fn factory_for(adapter_key: &str) -> Option<AdapterFactory> {
    FACTORIES
        .iter()
        .find_map(|(key, factory)| (*key == adapter_key).then_some(*factory))
}

/// Iterate every adapter key with a factory registered.
pub fn registered_keys() -> impl Iterator<Item = &'static str> {
    FACTORIES.iter().map(|(key, _)| *key)
}

/// Look up adapter metadata by key, ensuring the metadata + factory pair
/// remains in sync.
#[must_use]
pub fn metadata_for(adapter_key: &str) -> Option<&'static AdapterMetadata> {
    model_adapter::get(adapter_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_metadata_key_has_a_factory() {
        for key in model_adapter::keys() {
            assert!(
                factory_for(key).is_some(),
                "adapter metadata key `{key}` has no matching factory in fabro-llm",
            );
        }
    }

    #[test]
    fn every_factory_has_metadata() {
        for key in registered_keys() {
            assert!(
                metadata_for(key).is_some(),
                "fabro-llm factory `{key}` has no matching metadata in fabro-model",
            );
        }
    }

    #[test]
    fn registered_factory_set_matches_metadata_set() {
        let metadata: std::collections::BTreeSet<&str> = model_adapter::keys().collect();
        let factories: std::collections::BTreeSet<&str> = registered_keys().collect();
        assert_eq!(metadata, factories);
    }

    #[test]
    fn unknown_key_returns_none_factory() {
        assert!(factory_for("does_not_exist").is_none());
    }

    #[test]
    fn anthropic_factory_builds_anthropic_adapter() {
        let config = AdapterConfig::new("anthropic", ApiKeyHeader::Custom {
            name:  "x-api-key".to_string(),
            value: "test-key".to_string(),
        });
        let adapter = factory_for("anthropic").unwrap()(config);
        assert_eq!(adapter.name(), "anthropic");
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
        let adapter = factory_for("openai_compatible").unwrap()(config);
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

        let adapter = build_openai_compatible_adapter(config);

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
    #[should_panic(expected = "openai_compatible adapter requires a base_url")]
    fn openai_compatible_factory_panics_without_base_url() {
        let config = AdapterConfig::new("kimi", ApiKeyHeader::Bearer("k".to_string()));
        let _ = factory_for("openai_compatible").unwrap()(config);
    }
}
