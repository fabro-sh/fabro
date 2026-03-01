use crate::types::ModelInfo;
use std::sync::LazyLock;

/// Built-in model catalog loaded from catalog.json (Section 2.9).
/// The catalog is advisory, not restrictive -- unknown model strings pass through.
static BUILT_IN_MODELS: LazyLock<Vec<ModelInfo>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("catalog.json")).expect("embedded catalog.json must be valid")
});

/// Get model info by model ID (Section 2.9).
#[must_use]
pub fn get_model_info(model_id: &str) -> Option<ModelInfo> {
    BUILT_IN_MODELS
        .iter()
        .find(|m| m.id == model_id || m.aliases.iter().any(|a| a == model_id))
        .cloned()
}

/// List all known models, optionally filtered by provider (Section 2.9).
#[must_use]
pub fn list_models(provider: Option<&str>) -> Vec<ModelInfo> {
    provider.map_or_else(
        || BUILT_IN_MODELS.clone(),
        |p| {
            BUILT_IN_MODELS
                .iter()
                .filter(|m| m.provider == p)
                .cloned()
                .collect()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Provider;
    use std::str::FromStr;

    #[test]
    fn every_provider_has_catalog_models() {
        for &provider in Provider::ALL {
            let models = list_models(Some(provider.as_str()));
            assert!(
                !models.is_empty(),
                "Provider {:?} has no models in catalog",
                provider
            );
        }
    }

    #[test]
    fn catalog_provider_strings_roundtrip_through_provider() {
        for model in list_models(None) {
            let parsed = Provider::from_str(&model.provider);
            assert!(
                parsed.is_ok(),
                "catalog model '{}' has provider '{}' which does not parse as Provider",
                model.id,
                model.provider
            );
        }
    }

    #[test]
    fn provider_as_str_roundtrips_through_from_str() {
        for &provider in Provider::ALL {
            let roundtripped = Provider::from_str(provider.as_str());
            assert_eq!(
                roundtripped,
                Ok(provider),
                "Provider::{:?}.as_str() does not round-trip through from_str",
                provider
            );
        }
    }

    #[test]
    fn get_model_info_by_id() {
        let info = get_model_info("claude-opus-4-6").unwrap();
        assert_eq!(info.display_name, "Claude Opus 4.6");
        assert_eq!(info.provider, "anthropic");
        assert!(info.supports_tools);
        assert!(info.supports_vision);
        assert!(info.supports_reasoning);
        assert_eq!(info.context_window, 1_000_000);
        assert_eq!(info.max_output, Some(128_000));
    }

    #[test]
    fn get_model_info_by_alias() {
        let info = get_model_info("opus").unwrap();
        assert_eq!(info.id, "claude-opus-4-6");

        let info = get_model_info("sonnet").unwrap();
        assert_eq!(info.id, "claude-sonnet-4-5");

        let info = get_model_info("codex").unwrap();
        assert_eq!(info.id, "gpt-5.3-codex");
    }

    #[test]
    fn get_model_info_returns_none_for_unknown() {
        assert!(get_model_info("nonexistent-model").is_none());
    }

    #[test]
    fn list_models_all() {
        let models = list_models(None);
        assert_eq!(models.len(), 14);
    }

    #[test]
    fn list_models_by_provider() {
        let anthropic = list_models(Some("anthropic"));
        assert_eq!(anthropic.len(), 3);
        assert!(anthropic.iter().all(|m| m.provider == "anthropic"));

        let openai = list_models(Some("openai"));
        assert_eq!(openai.len(), 4);

        let gemini = list_models(Some("gemini"));
        assert_eq!(gemini.len(), 2);

        let unknown = list_models(Some("unknown"));
        assert!(unknown.is_empty());
    }

    #[test]
    fn kimi_k2_5_in_catalog() {
        let m = get_model_info("kimi-k2.5").unwrap();
        assert_eq!(m.provider, "kimi");
        assert_eq!(m.max_output, Some(16000));
        assert_eq!(m.context_window, 262144);
    }

    #[test]
    fn kimi_alias() {
        assert_eq!(get_model_info("kimi").unwrap().id, "kimi-k2.5");
    }

    #[test]
    fn glm_4_7_in_catalog() {
        let m = get_model_info("glm-4.7").unwrap();
        assert_eq!(m.provider, "zai");
    }

    #[test]
    fn minimax_m2_5_in_catalog() {
        let m = get_model_info("minimax-m2.5").unwrap();
        assert_eq!(m.provider, "minimax");
    }

    #[test]
    fn mercury_in_catalog() {
        let m = get_model_info("mercury").unwrap();
        assert_eq!(m.provider, "inception");
        assert_eq!(m.context_window, 32768);
        assert!(m.supports_tools);
        assert!(!m.supports_vision);
        assert!(!m.supports_reasoning);
    }

    #[test]
    fn mercury_coder_in_catalog() {
        let m = get_model_info("mercury-coder").unwrap();
        assert_eq!(m.provider, "inception");
    }

    #[test]
    fn model_info_costs() {
        let claude = get_model_info("claude-opus-4-6").unwrap();
        assert_eq!(claude.input_cost_per_million, Some(15.0));
        assert_eq!(claude.output_cost_per_million, Some(75.0));

        let sonnet = get_model_info("claude-sonnet-4-5").unwrap();
        assert_eq!(sonnet.input_cost_per_million, Some(3.0));
    }
}
