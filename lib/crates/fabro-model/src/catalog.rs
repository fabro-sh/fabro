use std::collections::HashMap;
use std::sync::LazyLock;

use crate::provider::Provider;
use crate::types::Model;

/// Global singleton catalog parsed from embedded catalog.json.
static GLOBAL_CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    let models: Vec<Model> = serde_json::from_str(include_str!("catalog.json"))
        .expect("embedded catalog.json must be valid");
    Catalog { models }
});

/// A resolved fallback target: provider name + model ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackTarget {
    pub provider: String,
    pub model: String,
}

/// Typed model catalog backed by a `Vec<Model>`.
///
/// Use [`Catalog::builtin()`] for the embedded catalog, or [`Catalog::from_models()`]
/// for testing with custom model sets.
pub struct Catalog {
    models: Vec<Model>,
}

impl Catalog {
    /// Returns a reference to the global built-in catalog (loaded once from catalog.json).
    #[must_use]
    pub fn builtin() -> &'static Self {
        &GLOBAL_CATALOG
    }

    /// Create a catalog from a custom set of models (useful for testing).
    #[must_use]
    pub fn from_models(models: Vec<Model>) -> Self {
        Self { models }
    }

    /// Look up a model by ID or alias.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Model> {
        self.models
            .iter()
            .find(|m| m.id == id || m.aliases.iter().any(|a| a == id))
    }

    /// List all models, optionally filtered by provider.
    #[must_use]
    pub fn list(&self, provider: Option<Provider>) -> Vec<&Model> {
        match provider {
            None => self.models.iter().collect(),
            Some(p) => self.models.iter().filter(|m| m.provider == p).collect(),
        }
    }

    /// The overall default model (first model marked `default` in catalog).
    ///
    /// # Panics
    /// Panics if the catalog contains no default model.
    #[must_use]
    pub fn default_model(&self) -> &Model {
        self.models
            .iter()
            .find(|m| m.default)
            .expect("catalog must contain at least one default model")
    }

    /// The default model for a specific provider.
    #[must_use]
    pub fn default_for_provider(&self, p: Provider) -> Option<&Model> {
        self.models.iter().find(|m| m.provider == p && m.default)
    }

    /// Default model for the best-available provider (based on API keys),
    /// falling back to the global catalog default.
    #[must_use]
    pub fn default_from_env(&self) -> &Model {
        let provider = Provider::default_from_env();
        self.default_for_provider(provider)
            .unwrap_or_else(|| self.default_model())
    }

    /// Probe model for a provider — the cheapest model suitable for connectivity checks.
    /// Falls back to the provider's default when no explicit override is configured.
    #[must_use]
    pub fn probe_for_provider(&self, p: Provider) -> Option<&Model> {
        let override_id: Option<&str> = match p {
            Provider::OpenAi => Some("gpt-5.4-mini"),
            _ => None,
        };
        if let Some(id) = override_id {
            if let Some(info) = self.get(id) {
                return Some(info);
            }
        }
        self.default_for_provider(p)
    }

    /// Find the closest model on a target provider matching the reference's capabilities.
    ///
    /// Hard-filters on `features.tools`, `features.vision`, and `features.reasoning`.
    /// Among matches, picks the closest by `costs.input_cost_per_mtok` (absolute diff).
    #[must_use]
    pub fn closest(&self, target: Provider, reference: &Model) -> Option<&Model> {
        self.models
            .iter()
            .filter(|m| {
                m.provider == target
                    && m.features.tools == reference.features.tools
                    && m.features.vision == reference.features.vision
                    && m.features.reasoning == reference.features.reasoning
            })
            .min_by(|a, b| {
                let ref_cost = reference.costs.input_cost_per_mtok.unwrap_or(0.0);
                let cost_a = (a.costs.input_cost_per_mtok.unwrap_or(0.0) - ref_cost).abs();
                let cost_b = (b.costs.input_cost_per_mtok.unwrap_or(0.0) - ref_cost).abs();
                cost_a
                    .partial_cmp(&cost_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// Build an ordered fallback chain for a primary provider/model.
    ///
    /// For each fallback provider, finds the closest matching model. Providers where
    /// no capability match exists (or the provider string doesn't parse) are skipped.
    #[must_use]
    pub fn build_fallback_chain(
        &self,
        primary: Provider,
        model: &str,
        fallbacks: &HashMap<String, Vec<String>>,
    ) -> Vec<FallbackTarget> {
        let Some(reference) = self.get(model) else {
            return Vec::new();
        };

        let Some(fallback_providers) = fallbacks.get(primary.as_str()) else {
            return Vec::new();
        };

        fallback_providers
            .iter()
            .filter_map(|provider_str| {
                let provider = provider_str.parse::<Provider>().ok()?;
                self.closest(provider, reference).map(|m| FallbackTarget {
                    provider: provider_str.clone(),
                    model: m.id.clone(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Provider;
    use std::str::FromStr;

    // ---- Catalog struct tests ----

    #[test]
    fn builtin_get_by_id() {
        let m = Catalog::builtin().get("claude-opus-4-6").unwrap();
        assert_eq!(m.id, "claude-opus-4-6");
    }

    #[test]
    fn builtin_get_by_alias() {
        let m = Catalog::builtin().get("opus").unwrap();
        assert_eq!(m.id, "claude-opus-4-6");
    }

    #[test]
    fn builtin_get_unknown() {
        assert!(Catalog::builtin().get("nonexistent").is_none());
    }

    #[test]
    fn builtin_list_all() {
        let all = Catalog::builtin().list(None);
        assert!(!all.is_empty());
    }

    #[test]
    fn builtin_list_by_provider() {
        let anthropic = Catalog::builtin().list(Some(Provider::Anthropic));
        assert!(!anthropic.is_empty());
        assert!(anthropic.iter().all(|m| m.provider == Provider::Anthropic));
    }

    #[test]
    fn builtin_list_unknown_provider_empty() {
        // OpenAiCompatible has no catalog models
        let models = Catalog::builtin().list(Some(Provider::OpenAiCompatible));
        assert!(models.is_empty());
    }

    #[test]
    fn builtin_default_model() {
        let m = Catalog::builtin().default_model();
        assert!(m.default);
    }

    #[test]
    fn builtin_default_for_provider() {
        let m = Catalog::builtin()
            .default_for_provider(Provider::Anthropic)
            .unwrap();
        assert_eq!(m.id, "claude-sonnet-4-6");
        assert!(m.default);

        let m = Catalog::builtin()
            .default_for_provider(Provider::OpenAi)
            .unwrap();
        assert_eq!(m.id, "gpt-5.4");

        let m = Catalog::builtin()
            .default_for_provider(Provider::Gemini)
            .unwrap();
        assert_eq!(m.id, "gemini-3.1-pro-preview");
    }

    #[test]
    fn builtin_probe_openai_returns_override() {
        let m = Catalog::builtin()
            .probe_for_provider(Provider::OpenAi)
            .unwrap();
        assert_eq!(m.id, "gpt-5.4-mini");
    }

    #[test]
    fn builtin_probe_anthropic_returns_default() {
        let m = Catalog::builtin()
            .probe_for_provider(Provider::Anthropic)
            .unwrap();
        assert_eq!(m.id, "claude-sonnet-4-6");
    }

    #[test]
    fn builtin_probe_gemini_returns_default() {
        let m = Catalog::builtin()
            .probe_for_provider(Provider::Gemini)
            .unwrap();
        assert_eq!(m.id, "gemini-3.1-pro-preview");
    }

    #[test]
    fn builtin_closest_opus_to_gemini() {
        let opus = Catalog::builtin().get("claude-opus-4-6").unwrap();
        let result = Catalog::builtin().closest(Provider::Gemini, opus).unwrap();
        assert_eq!(result.id, "gemini-3.1-pro-preview");
    }

    #[test]
    fn builtin_closest_no_match() {
        let haiku = Catalog::builtin().get("claude-haiku-4-5").unwrap();
        assert!(
            Catalog::builtin()
                .closest(Provider::OpenAi, haiku)
                .is_none()
        );
    }

    #[test]
    fn builtin_build_fallback_chain() {
        let fallbacks = HashMap::from([(
            "anthropic".to_string(),
            vec!["gemini".to_string(), "openai".to_string()],
        )]);
        let chain = Catalog::builtin().build_fallback_chain(
            Provider::Anthropic,
            "claude-opus-4-6",
            &fallbacks,
        );
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].provider, "gemini");
        assert_eq!(chain[0].model, "gemini-3.1-pro-preview");
        assert_eq!(chain[1].provider, "openai");
        assert_eq!(chain[1].model, "gpt-5.4");
    }

    #[test]
    fn builtin_build_fallback_chain_unknown_model() {
        let fallbacks = HashMap::from([("anthropic".to_string(), vec!["gemini".to_string()])]);
        let chain =
            Catalog::builtin().build_fallback_chain(Provider::Anthropic, "unknown-xyz", &fallbacks);
        assert!(chain.is_empty());
    }

    #[test]
    fn builtin_build_fallback_chain_provider_not_in_map() {
        let fallbacks = HashMap::from([("openai".to_string(), vec!["anthropic".to_string()])]);
        let chain = Catalog::builtin().build_fallback_chain(
            Provider::Anthropic,
            "claude-opus-4-6",
            &fallbacks,
        );
        assert!(chain.is_empty());
    }

    #[test]
    fn builtin_build_fallback_chain_skips_no_capability_match() {
        let fallbacks = HashMap::from([(
            "anthropic".to_string(),
            vec!["openai".to_string(), "kimi".to_string()],
        )]);
        let chain = Catalog::builtin().build_fallback_chain(
            Provider::Anthropic,
            "claude-haiku-4-5",
            &fallbacks,
        );
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].provider, "kimi");
        assert_eq!(chain[0].model, "kimi-k2.5");
    }

    #[test]
    fn builtin_build_fallback_chain_empty_map() {
        let fallbacks = HashMap::new();
        let chain = Catalog::builtin().build_fallback_chain(
            Provider::Anthropic,
            "claude-opus-4-6",
            &fallbacks,
        );
        assert!(chain.is_empty());
    }

    #[test]
    fn from_models_custom_catalog() {
        use crate::types::{Model, ModelCosts, ModelFeatures, ModelLimits};

        let models = vec![Model {
            id: "test-model".to_string(),
            provider: Provider::Anthropic,
            family: "test".to_string(),
            display_name: "Test Model".to_string(),
            limits: ModelLimits {
                context_window: 100_000,
                max_output: Some(4096),
            },
            training: None,
            knowledge_cutoff: None,
            features: ModelFeatures {
                tools: true,
                vision: false,
                reasoning: false,
                effort: false,
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(1.0),
                output_cost_per_mtok: Some(5.0),
                cache_input_cost_per_mtok: None,
            },
            estimated_output_tps: None,
            aliases: vec!["test".to_string()],
            default: true,
        }];

        let catalog = Catalog::from_models(models);
        assert_eq!(catalog.get("test-model").unwrap().id, "test-model");
        assert_eq!(catalog.get("test").unwrap().id, "test-model");
        assert!(catalog.get("nonexistent").is_none());
        assert_eq!(catalog.default_model().id, "test-model");
        assert_eq!(catalog.list(None).len(), 1);
    }

    // ---- Provider / catalog data integrity tests ----

    #[test]
    fn every_provider_has_catalog_models() {
        for &provider in Provider::ALL {
            let models = Catalog::builtin().list(Some(provider));
            assert!(
                !models.is_empty(),
                "Provider {:?} has no models in catalog",
                provider
            );
        }
    }

    #[test]
    fn every_provider_has_exactly_one_default_model() {
        for &provider in Provider::ALL {
            let defaults: Vec<_> = Catalog::builtin()
                .list(Some(provider))
                .into_iter()
                .filter(|m| m.default)
                .collect();
            assert_eq!(
                defaults.len(),
                1,
                "Provider {:?} should have exactly one default model, found {}: {:?}",
                provider,
                defaults.len(),
                defaults.iter().map(|m| &m.id).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn catalog_providers_roundtrip_through_as_str() {
        for model in Catalog::builtin().list(None) {
            let roundtripped = Provider::from_str(model.provider.as_str());
            assert_eq!(
                roundtripped,
                Ok(model.provider),
                "catalog model '{}' provider {:?} does not roundtrip through as_str",
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

    // ---- Model info snapshot tests ----

    #[test]
    fn get_model_info_by_id() {
        let info = Catalog::builtin().get("claude-opus-4-6").unwrap();
        insta::assert_debug_snapshot!(info, @r#"
        Model {
            id: "claude-opus-4-6",
            provider: Anthropic,
            family: "claude-4",
            display_name: "Claude Opus 4.6",
            limits: ModelLimits {
                context_window: 1000000,
                max_output: Some(
                    128000,
                ),
            },
            training: Some(
                "2025-08-01",
            ),
            knowledge_cutoff: Some(
                "May 2025",
            ),
            features: ModelFeatures {
                tools: true,
                vision: true,
                reasoning: true,
                effort: true,
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(
                    15.0,
                ),
                output_cost_per_mtok: Some(
                    75.0,
                ),
                cache_input_cost_per_mtok: Some(
                    1.5,
                ),
            },
            estimated_output_tps: Some(
                25.0,
            ),
            aliases: [
                "opus",
                "claude-opus",
            ],
            default: false,
        }
        "#);
    }

    #[test]
    fn get_model_info_by_alias() {
        assert_eq!(
            Catalog::builtin().get("opus").unwrap().id,
            "claude-opus-4-6"
        );
        assert_eq!(
            Catalog::builtin().get("sonnet").unwrap().id,
            "claude-sonnet-4-6"
        );
        assert_eq!(Catalog::builtin().get("codex").unwrap().id, "gpt-5.3-codex");
    }

    #[test]
    fn get_model_info_returns_none_for_unknown() {
        assert!(Catalog::builtin().get("nonexistent-model").is_none());
    }

    #[test]
    fn gemini_3_1_flash_lite_in_catalog() {
        let m = Catalog::builtin()
            .get("gemini-3.1-flash-lite-preview")
            .unwrap();
        insta::assert_debug_snapshot!(m, @r#"
        Model {
            id: "gemini-3.1-flash-lite-preview",
            provider: Gemini,
            family: "gemini-3",
            display_name: "Gemini 3.1 Flash Lite (Preview)",
            limits: ModelLimits {
                context_window: 1048576,
                max_output: Some(
                    65536,
                ),
            },
            training: Some(
                "2025-01-01",
            ),
            knowledge_cutoff: Some(
                "January 2025",
            ),
            features: ModelFeatures {
                tools: true,
                vision: true,
                reasoning: true,
                effort: true,
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(
                    0.25,
                ),
                output_cost_per_mtok: Some(
                    1.5,
                ),
                cache_input_cost_per_mtok: Some(
                    0.0625,
                ),
            },
            estimated_output_tps: Some(
                200.0,
            ),
            aliases: [
                "gemini-flash-lite",
            ],
            default: false,
        }
        "#);
    }

    #[test]
    fn gemini_flash_lite_alias() {
        assert_eq!(
            Catalog::builtin().get("gemini-flash-lite").unwrap().id,
            "gemini-3.1-flash-lite-preview"
        );
    }

    #[test]
    fn kimi_k2_5_in_catalog() {
        let m = Catalog::builtin().get("kimi-k2.5").unwrap();
        insta::assert_debug_snapshot!(m, @r#"
        Model {
            id: "kimi-k2.5",
            provider: Kimi,
            family: "kimi-k2",
            display_name: "Kimi K2.5",
            limits: ModelLimits {
                context_window: 262144,
                max_output: Some(
                    16000,
                ),
            },
            training: Some(
                "2025-10-01",
            ),
            knowledge_cutoff: Some(
                "October 2025",
            ),
            features: ModelFeatures {
                tools: true,
                vision: true,
                reasoning: false,
                effort: false,
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(
                    0.6,
                ),
                output_cost_per_mtok: Some(
                    3.0,
                ),
                cache_input_cost_per_mtok: None,
            },
            estimated_output_tps: Some(
                50.0,
            ),
            aliases: [
                "kimi",
            ],
            default: true,
        }
        "#);
    }

    #[test]
    fn kimi_alias() {
        assert_eq!(Catalog::builtin().get("kimi").unwrap().id, "kimi-k2.5");
    }

    #[test]
    fn glm_4_7_in_catalog() {
        let m = Catalog::builtin().get("glm-4.7").unwrap();
        assert_eq!(m.provider, Provider::Zai);
    }

    #[test]
    fn minimax_m2_5_in_catalog() {
        let m = Catalog::builtin().get("minimax-m2.5").unwrap();
        assert_eq!(m.provider, Provider::Minimax);
    }

    #[test]
    fn mercury_2_in_catalog() {
        let m = Catalog::builtin().get("mercury-2").unwrap();
        insta::assert_debug_snapshot!(m, @r#"
        Model {
            id: "mercury-2",
            provider: Inception,
            family: "mercury",
            display_name: "Mercury 2",
            limits: ModelLimits {
                context_window: 131072,
                max_output: Some(
                    50000,
                ),
            },
            training: None,
            knowledge_cutoff: None,
            features: ModelFeatures {
                tools: true,
                vision: false,
                reasoning: true,
                effort: true,
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(
                    0.25,
                ),
                output_cost_per_mtok: Some(
                    0.75,
                ),
                cache_input_cost_per_mtok: None,
            },
            estimated_output_tps: Some(
                1000.0,
            ),
            aliases: [
                "mercury",
            ],
            default: true,
        }
        "#);
    }

    #[test]
    fn mercury_alias_resolves_to_mercury_2() {
        assert_eq!(Catalog::builtin().get("mercury").unwrap().id, "mercury-2");
    }

    #[test]
    fn gpt_5_4_in_catalog() {
        let m = Catalog::builtin().get("gpt-5.4").unwrap();
        insta::assert_debug_snapshot!(m, @r#"
        Model {
            id: "gpt-5.4",
            provider: OpenAi,
            family: "gpt-5",
            display_name: "GPT-5.4",
            limits: ModelLimits {
                context_window: 1047576,
                max_output: Some(
                    128000,
                ),
            },
            training: Some(
                "2025-08-31",
            ),
            knowledge_cutoff: Some(
                "April 2025",
            ),
            features: ModelFeatures {
                tools: true,
                vision: true,
                reasoning: true,
                effort: true,
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(
                    2.5,
                ),
                output_cost_per_mtok: Some(
                    15.0,
                ),
                cache_input_cost_per_mtok: Some(
                    0.25,
                ),
            },
            estimated_output_tps: Some(
                70.0,
            ),
            aliases: [
                "gpt54",
                "gpt-54",
            ],
            default: true,
        }
        "#);
    }

    #[test]
    fn gpt_5_4_pro_in_catalog() {
        let m = Catalog::builtin().get("gpt-5.4-pro").unwrap();
        insta::assert_debug_snapshot!(m, @r#"
        Model {
            id: "gpt-5.4-pro",
            provider: OpenAi,
            family: "gpt-5",
            display_name: "GPT-5.4 Pro",
            limits: ModelLimits {
                context_window: 1047576,
                max_output: Some(
                    128000,
                ),
            },
            training: Some(
                "2025-08-31",
            ),
            knowledge_cutoff: Some(
                "April 2025",
            ),
            features: ModelFeatures {
                tools: true,
                vision: true,
                reasoning: true,
                effort: true,
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(
                    30.0,
                ),
                output_cost_per_mtok: Some(
                    180.0,
                ),
                cache_input_cost_per_mtok: Some(
                    3.0,
                ),
            },
            estimated_output_tps: Some(
                20.0,
            ),
            aliases: [
                "gpt54-pro",
                "gpt-54-pro",
            ],
            default: false,
        }
        "#);
    }

    #[test]
    fn gpt54_alias() {
        assert_eq!(Catalog::builtin().get("gpt54").unwrap().id, "gpt-5.4");
    }

    #[test]
    fn gpt_54_hyphenated_alias() {
        assert_eq!(Catalog::builtin().get("gpt-54").unwrap().id, "gpt-5.4");
    }

    #[test]
    fn gpt_54_pro_hyphenated_alias() {
        assert_eq!(
            Catalog::builtin().get("gpt-54-pro").unwrap().id,
            "gpt-5.4-pro"
        );
    }

    #[test]
    fn gpt_54_mini_hyphenated_alias() {
        assert_eq!(
            Catalog::builtin().get("gpt-54-mini").unwrap().id,
            "gpt-5.4-mini"
        );
    }

    #[test]
    fn gpt_5_3_codex_spark_in_catalog() {
        let m = Catalog::builtin().get("gpt-5.3-codex-spark").unwrap();
        insta::assert_debug_snapshot!(m, @r#"
        Model {
            id: "gpt-5.3-codex-spark",
            provider: OpenAi,
            family: "gpt-5",
            display_name: "GPT-5.3 Codex Spark",
            limits: ModelLimits {
                context_window: 131072,
                max_output: Some(
                    128000,
                ),
            },
            training: Some(
                "2025-08-31",
            ),
            knowledge_cutoff: Some(
                "April 2025",
            ),
            features: ModelFeatures {
                tools: true,
                vision: false,
                reasoning: true,
                effort: true,
            },
            costs: ModelCosts {
                input_cost_per_mtok: None,
                output_cost_per_mtok: None,
                cache_input_cost_per_mtok: None,
            },
            estimated_output_tps: Some(
                1000.0,
            ),
            aliases: [
                "codex-spark",
            ],
            default: false,
        }
        "#);
    }

    #[test]
    fn codex_spark_alias() {
        assert_eq!(
            Catalog::builtin().get("codex-spark").unwrap().id,
            "gpt-5.3-codex-spark"
        );
    }

    // ---- Closest model tests ----

    #[test]
    fn closest_model_sonnet_to_gemini() {
        let sonnet = Catalog::builtin().get("claude-sonnet-4-5").unwrap();
        let result = Catalog::builtin()
            .closest(Provider::Gemini, sonnet)
            .unwrap();
        assert_eq!(result.id, "gemini-3.1-pro-preview");
    }

    #[test]
    fn closest_model_haiku_to_kimi() {
        let haiku = Catalog::builtin().get("claude-haiku-4-5").unwrap();
        let result = Catalog::builtin().closest(Provider::Kimi, haiku).unwrap();
        assert_eq!(result.id, "kimi-k2.5");
    }

    #[test]
    fn closest_model_no_capability_match() {
        let glm = Catalog::builtin().get("glm-4.7").unwrap();
        assert!(Catalog::builtin().closest(Provider::Gemini, glm).is_none());
    }

    // ---- Cost tests ----

    #[test]
    fn model_info_costs() {
        let claude = Catalog::builtin().get("claude-opus-4-6").unwrap();
        assert_eq!(claude.costs.input_cost_per_mtok, Some(15.0));
        assert_eq!(claude.costs.output_cost_per_mtok, Some(75.0));

        let sonnet = Catalog::builtin().get("claude-sonnet-4-5").unwrap();
        assert_eq!(sonnet.costs.input_cost_per_mtok, Some(3.0));
    }
}
