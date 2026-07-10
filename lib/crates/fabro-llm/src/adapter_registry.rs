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
use fabro_model::{AdapterKind, AgentProfileKind, BillingPolicy, Catalog, CodecKind, ProviderId};

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
    /// Adapter-kind-specific options; factories for other kinds ignore
    /// options that are not theirs.
    pub kind_options:  AdapterKindOptions,
    pub catalog:       Option<Arc<Catalog>>,
}

/// Construction options that only apply to one adapter kind, kept out of the
/// shared [`AdapterConfig`] fields.
#[derive(Debug, Clone, Default)]
pub enum AdapterKindOptions {
    /// No kind-specific options.
    #[default]
    None,
    OpenAi(OpenAiAdapterOptions),
}

/// OpenAI-only construction options.
#[derive(Debug, Clone, Default)]
pub struct OpenAiAdapterOptions {
    /// Route through the ChatGPT Codex backend.
    pub codex_mode: bool,
    /// Organization ID.
    pub org_id:     Option<String>,
    /// Project ID.
    pub project_id: Option<String>,
}

impl AdapterConfig {
    /// Construct a minimal config with just provider ID and auth header.
    pub fn new(provider_id: impl Into<String>, auth_header: ApiKeyHeader) -> Self {
        Self {
            provider_id:   provider_id.into(),
            auth_header:   Some(auth_header),
            base_url:      None,
            extra_headers: HashMap::new(),
            kind_options:  AdapterKindOptions::None,
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
        // SigV4 is not a static header; only the Bedrock adapter consumes
        // the marker (it signs at request time).
        Some(ApiKeyHeader::AwsSigv4) | None => None,
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
    let options = match config.kind_options {
        AdapterKindOptions::OpenAi(options) => options,
        AdapterKindOptions::None => OpenAiAdapterOptions::default(),
    };
    let mut adapter =
        providers::OpenAiAdapter::new_optional_auth(api_key).with_name(config.provider_id.clone());
    if let Some(base_url) = config.base_url {
        adapter = adapter.with_base_url(base_url);
    }
    if !config.extra_headers.is_empty() {
        adapter = adapter.with_default_headers(config.extra_headers);
    }
    if options.codex_mode {
        adapter = adapter.with_codex_mode();
    }
    if let Some(org_id) = options.org_id {
        adapter = adapter.with_org_id(org_id);
    }
    if let Some(project_id) = options.project_id {
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
        AdapterKind::Bedrock => providers::bedrock::build,
    }
}

/// A resolved route for one catalog model: the transport+auth key, wire
/// dialect, and provider-facing identifiers a request for that model travels
/// with.
///
/// `(provider row, model row)` → route. Codec/transport pairings are
/// validated at catalog build, so any model in a successfully built catalog
/// resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Canonical provider this route belongs to.
    pub provider:       ProviderId,
    /// Transport + auth scheme (the adapter registry key).
    pub transport:      AdapterKind,
    /// Wire dialect spoken on this route.
    pub codec:          CodecKind,
    /// Identifier sent to the provider API (the catalog `api_id`).
    pub deployment_id:  String,
    /// Billing family used to translate usage into billed tokens.
    pub billing_policy: BillingPolicy,
    /// Agent profile driving profile-specific behavior.
    pub agent_profile:  AgentProfileKind,
}

/// Resolve the route for `model_id_or_alias` from the catalog's provider and
/// model rows. Returns `None` when the model or its provider is unknown.
#[must_use]
pub fn resolve_route(catalog: &Catalog, model_id_or_alias: &str) -> Option<Route> {
    let model = catalog.get(model_id_or_alias)?;
    let provider = catalog.provider(&model.provider)?;
    let settings = catalog.model_settings(&model.id)?;
    Some(Route {
        provider:       provider.id.clone(),
        transport:      provider.adapter,
        codec:          settings.codec,
        deployment_id:  settings.api_id.clone(),
        billing_policy: settings.billing_policy,
        agent_profile:  settings.agent_profile,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the route-equivalence table: model id plus the
    /// `(deployment_id, transport, codec, billing_policy, agent_profile)`
    /// tuple it must resolve to.
    type RouteRow = (
        &'static str,
        &'static str,
        AdapterKind,
        CodecKind,
        BillingPolicy,
        AgentProfileKind,
    );

    /// The compat mapping as an executable table: every built-in catalog
    /// model resolves to exactly this tuple. Adding or rerouting a built-in
    /// model means updating this table deliberately.
    #[test]
    fn builtin_catalog_route_equivalence_table() {
        use AdapterKind as T;
        use AgentProfileKind as P;
        use BillingPolicy as B;
        use CodecKind as C;

        #[rustfmt::skip]
        let expected: &[RouteRow] = &[
            // model id                            deployment_id                          transport             codec                 billing       profile
            ("claude-fable-5",                     "claude-fable-5",                      T::Anthropic,         C::AnthropicMessages, B::Anthropic, P::Anthropic),
            ("claude-haiku-4-5",                   "claude-haiku-4-5",                    T::Anthropic,         C::AnthropicMessages, B::Anthropic, P::Anthropic),
            ("claude-opus-4-6",                    "claude-opus-4-6",                     T::Anthropic,         C::AnthropicMessages, B::Anthropic, P::Anthropic),
            ("claude-opus-4-7",                    "claude-opus-4-7",                     T::Anthropic,         C::AnthropicMessages, B::Anthropic, P::Anthropic),
            ("claude-opus-4-8",                    "claude-opus-4-8",                     T::Anthropic,         C::AnthropicMessages, B::Anthropic, P::Anthropic),
            ("claude-sonnet-4-5",                  "claude-sonnet-4-5",                   T::Anthropic,         C::AnthropicMessages, B::Anthropic, P::Anthropic),
            ("claude-sonnet-4-6",                  "claude-sonnet-4-6",                   T::Anthropic,         C::AnthropicMessages, B::Anthropic, P::Anthropic),
            ("gemini-3-flash-preview",             "gemini-3-flash-preview",              T::Gemini,            C::GeminiGenerate,    B::Gemini,    P::Gemini),
            ("gemini-3.1-flash-lite",              "gemini-3.1-flash-lite",               T::Gemini,            C::GeminiGenerate,    B::Gemini,    P::Gemini),
            ("gemini-3.1-pro-preview",             "gemini-3.1-pro-preview",              T::Gemini,            C::GeminiGenerate,    B::Gemini,    P::Gemini),
            ("gemini-3.1-pro-preview-customtools", "gemini-3.1-pro-preview-customtools",  T::Gemini,            C::GeminiGenerate,    B::Gemini,    P::Gemini),
            ("gemini-3.5-flash",                   "gemini-3.5-flash",                    T::Gemini,            C::GeminiGenerate,    B::Gemini,    P::Gemini),
            ("glm-4.7",                            "glm-4.7",                             T::OpenAiCompatible,  C::OpenAiCompatible,  B::OpenAi,    P::OpenAi),
            ("gpt-5.4",                            "gpt-5.4",                             T::OpenAi,            C::OpenAiResponses,   B::OpenAi,    P::OpenAi),
            ("gpt-5.4-mini",                       "gpt-5.4-mini",                        T::OpenAi,            C::OpenAiResponses,   B::OpenAi,    P::OpenAi),
            ("gpt-5.4-pro",                        "gpt-5.4-pro",                         T::OpenAi,            C::OpenAiResponses,   B::OpenAi,    P::OpenAi),
            ("gpt-5.5",                            "gpt-5.5",                             T::OpenAi,            C::OpenAiResponses,   B::OpenAi,    P::OpenAi),
            ("gpt-5.5-pro",                        "gpt-5.5-pro",                         T::OpenAi,            C::OpenAiResponses,   B::OpenAi,    P::OpenAi),
            ("gpt-5.6-luna",                       "gpt-5.6-luna",                        T::OpenAi,            C::OpenAiResponses,   B::OpenAi,    P::OpenAi),
            ("gpt-5.6-sol",                        "gpt-5.6-sol",                         T::OpenAi,            C::OpenAiResponses,   B::OpenAi,    P::OpenAi),
            ("gpt-5.6-terra",                      "gpt-5.6-terra",                       T::OpenAi,            C::OpenAiResponses,   B::OpenAi,    P::OpenAi),
            ("kimi-k2.5",                          "kimi-k2.5",                           T::OpenAiCompatible,  C::OpenAiCompatible,  B::OpenAi,    P::OpenAi),
            ("mercury-2",                          "mercury-2",                           T::OpenAiCompatible,  C::OpenAiCompatible,  B::OpenAi,    P::OpenAi),
            ("minimax-m2.5",                       "minimax-m2.5",                        T::OpenAiCompatible,  C::OpenAiCompatible,  B::OpenAi,    P::OpenAi),
            ("venice-uncensored-1-2",              "venice-uncensored-1-2",               T::OpenAiCompatible,  C::OpenAiCompatible,  B::OpenAi,    P::OpenAi),
            ("venice-uncensored-role-play",        "venice-uncensored-role-play",         T::OpenAiCompatible,  C::OpenAiCompatible,  B::OpenAi,    P::OpenAi),
        ];

        let catalog = Catalog::builtin();

        let mut model_ids: Vec<&str> = catalog
            .list(None)
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        model_ids.sort_unstable();
        let mut expected_ids: Vec<&str> = expected.iter().map(|row| row.0).collect();
        expected_ids.sort_unstable();
        assert_eq!(
            model_ids, expected_ids,
            "route-equivalence table must cover every built-in model row"
        );

        for (model_id, deployment_id, transport, codec, billing_policy, agent_profile) in expected {
            let route = resolve_route(catalog, model_id)
                .unwrap_or_else(|| panic!("built-in model '{model_id}' should resolve"));
            assert_eq!(route.deployment_id, *deployment_id, "{model_id}");
            assert_eq!(route.transport, *transport, "{model_id}");
            assert_eq!(route.codec, *codec, "{model_id}");
            assert_eq!(route.billing_policy, *billing_policy, "{model_id}");
            assert_eq!(route.agent_profile, *agent_profile, "{model_id}");
        }
    }

    #[test]
    fn resolve_route_follows_model_aliases() {
        let catalog = Catalog::builtin();

        let by_alias = resolve_route(catalog, "sonnet").expect("alias should resolve");
        let by_id = resolve_route(catalog, "claude-sonnet-4-6").expect("id should resolve");

        assert_eq!(by_alias, by_id);
        assert_eq!(by_alias.provider, ProviderId::anthropic());
    }

    #[test]
    fn resolve_route_returns_none_for_unknown_models() {
        assert_eq!(resolve_route(Catalog::builtin(), "not-a-model"), None);
    }

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
            base_url: Some("https://api.custom.test/v1".to_string()),
            extra_headers: HashMap::from([("x-api-key".to_string(), "secondary-key".to_string())]),
            ..AdapterConfig::new("custom", ApiKeyHeader::Custom {
                name:  "x-api-key".to_string(),
                value: "primary-key".to_string(),
            })
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
            base_url: Some("https://api.moonshot.ai/v1".to_string()),
            ..AdapterConfig::new("kimi", ApiKeyHeader::Bearer("k".to_string()))
        };
        let adapter = factory_for(AdapterKind::OpenAiCompatible)(config).unwrap();
        assert_eq!(adapter.name(), "kimi");
    }

    #[test]
    fn openai_compatible_factory_preserves_extra_headers() {
        let config = AdapterConfig {
            base_url: Some("https://api.portkey.ai/v1".to_string()),
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
            ..AdapterConfig::new(
                "portkey",
                ApiKeyHeader::Bearer("unused-primary-key".to_string()),
            )
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
            base_url: Some("https://api.portkey.ai/v1".to_string()),
            extra_headers: HashMap::from([(
                "x-portkey-api-key".to_string(),
                "resolved-portkey-key".to_string(),
            )]),
            ..AdapterConfig::new("anthropic-through-portkey", ApiKeyHeader::Custom {
                name:  "x-api-key".to_string(),
                value: "unused-primary-key".to_string(),
            })
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
