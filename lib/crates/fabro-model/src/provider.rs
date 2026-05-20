use serde::{Deserialize, Serialize};

use crate::adapter::AdapterKind;
use crate::catalog::CatalogProvider;
use crate::ids::ProviderId;

/// A user-facing LLM provider from the catalog.
///
/// The public projection of [`CatalogProvider`]. It deliberately omits
/// internal-only fields (`auth`, `extra_headers`, `billing_policy`,
/// `agent_profile`) so credential material never reaches the wire — the same
/// separation that already exists between catalog model settings and
/// [`crate::Model`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provider {
    pub id:            ProviderId,
    pub display_name:  String,
    pub adapter:       AdapterKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url:      Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_url:   Option<String>,
    pub priority:      i32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases:       Vec<String>,
    /// Number of catalog models for this provider. Stamped by the handler.
    pub model_count:   u32,
    /// Catalog default model ID for this provider, if any. Stamped by the
    /// handler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// True if the server has credential material configured for this provider
    /// when the response is produced. Always `false` in static catalog data;
    /// stamped by `GET /providers` per request.
    #[serde(default)]
    pub configured:    bool,
}

impl From<&CatalogProvider> for Provider {
    /// Builds the static fields from the catalog provider. `model_count`,
    /// `default_model`, and `configured` are left at their defaults for the
    /// handler to stamp.
    fn from(provider: &CatalogProvider) -> Self {
        Self {
            id:            provider.id.clone(),
            display_name:  provider.display_name.clone(),
            adapter:       provider.adapter,
            base_url:      provider.base_url.clone(),
            api_key_url:   provider.api_key_url.clone(),
            priority:      provider.priority,
            aliases:       provider.aliases.clone(),
            model_count:   0,
            default_model: None,
            configured:    false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Provider;
    use crate::adapter::AdapterKind;
    use crate::catalog::Catalog;
    use crate::ids::ProviderId;

    #[test]
    fn from_catalog_provider_copies_static_fields_and_defaults_dynamic_ones() {
        let catalog = Catalog::builtin();
        let anthropic = catalog
            .provider(&ProviderId::anthropic())
            .expect("builtin catalog must define anthropic");

        let provider = Provider::from(anthropic);

        assert_eq!(provider.id, ProviderId::anthropic());
        assert_eq!(provider.display_name, anthropic.display_name);
        assert_eq!(provider.adapter, anthropic.adapter);
        assert_eq!(provider.priority, anthropic.priority);
        // Dynamic fields are left for the handler to stamp.
        assert_eq!(provider.model_count, 0);
        assert_eq!(provider.default_model, None);
        assert!(!provider.configured);
    }

    #[test]
    fn optional_fields_are_skipped_when_absent() {
        let provider = Provider {
            id:            ProviderId::new("custom"),
            display_name:  "Custom".to_string(),
            adapter:       AdapterKind::OpenAiCompatible,
            base_url:      None,
            api_key_url:   None,
            priority:      0,
            aliases:       Vec::new(),
            model_count:   0,
            default_model: None,
            configured:    false,
        };

        let json = serde_json::to_value(&provider).unwrap();
        let object = json.as_object().unwrap();
        assert!(!object.contains_key("base_url"));
        assert!(!object.contains_key("api_key_url"));
        assert!(!object.contains_key("aliases"));
        assert!(!object.contains_key("default_model"));
        // `configured` has no skip and must always serialize.
        assert_eq!(object["configured"], serde_json::json!(false));
    }
}
