use serde::{Deserialize, Serialize};

use crate::provider::Provider;

// --- 2.9 Model ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelLimits {
    pub context_window: i64,
    pub max_output:     Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelFeatures {
    pub tools:     bool,
    pub vision:    bool,
    pub reasoning: bool,
    /// Whether the model supports the `reasoning_effort` / `effort` parameter
    /// directly (e.g. Anthropic `output_config.effort`, OpenAI
    /// `reasoning.effort`). Models with `reasoning=true` but `effort=false`
    /// (e.g. claude-sonnet-4-5) need the older `thinking` API with
    /// `budget_tokens` instead.
    #[serde(default)]
    pub effort:    bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCosts {
    pub input_cost_per_mtok:       Option<f64>,
    pub output_cost_per_mtok:      Option<f64>,
    pub cache_input_cost_per_mtok: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub id:                   String,
    pub provider:             Provider,
    pub family:               String,
    pub display_name:         String,
    pub limits:               ModelLimits,
    pub training:             Option<String>,
    pub knowledge_cutoff:     Option<String>,
    pub features:             ModelFeatures,
    pub costs:                ModelCosts,
    pub estimated_output_tps: Option<f64>,
    pub aliases:              Vec<String>,
    #[serde(default)]
    pub default:              bool,
    /// Whether the server has any credential configured for this model's
    /// provider at the time of the response. Always `false` in static catalog
    /// data; populated by `GET /models` per request.
    #[serde(default)]
    pub configured:           bool,
}

impl Model {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn provider(&self) -> Provider {
        self.provider
    }

    pub fn family(&self) -> &str {
        &self.family
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn context_window(&self) -> i64 {
        self.limits.context_window
    }

    pub fn max_output(&self) -> Option<i64> {
        self.limits.max_output
    }

    pub fn supports_tools(&self) -> bool {
        self.features.tools
    }

    pub fn supports_vision(&self) -> bool {
        self.features.vision
    }

    pub fn supports_reasoning(&self) -> bool {
        self.features.reasoning
    }

    pub fn supports_effort(&self) -> bool {
        self.features.effort
    }

    pub fn training(&self) -> Option<&str> {
        self.training.as_deref()
    }

    pub fn knowledge_cutoff(&self) -> Option<&str> {
        self.knowledge_cutoff.as_deref()
    }

    pub fn input_cost_per_mtok(&self) -> Option<f64> {
        self.costs.input_cost_per_mtok
    }

    pub fn output_cost_per_mtok(&self) -> Option<f64> {
        self.costs.output_cost_per_mtok
    }

    pub fn cache_input_cost_per_mtok(&self) -> Option<f64> {
        self.costs.cache_input_cost_per_mtok
    }

    pub fn estimated_output_tps(&self) -> Option<f64> {
        self.estimated_output_tps
    }

    pub fn aliases(&self) -> &[String] {
        &self.aliases
    }

    pub fn is_default(&self) -> bool {
        self.default
    }
}

#[cfg(test)]
mod tests {
    use crate::catalog::Catalog;
    use crate::provider::Provider;

    #[test]
    fn inherent_methods_return_correct_values() {
        let info = Catalog::builtin().get("claude-opus-4-7").unwrap();
        assert_eq!(info.id(), "claude-opus-4-7");
        assert_eq!(info.provider(), Provider::Anthropic);
        assert_eq!(info.family(), "claude-4");
        assert_eq!(info.display_name(), "Claude Opus 4.7");
        assert_eq!(info.context_window(), 1_000_000);
        assert_eq!(info.max_output(), Some(128_000));
        assert!(info.supports_tools());
        assert!(info.supports_vision());
        assert!(info.supports_reasoning());
        assert!(info.supports_effort());
        assert_eq!(info.training(), Some("2025-08-01"));
        assert_eq!(info.knowledge_cutoff(), Some("May 2025"));
        assert_eq!(info.input_cost_per_mtok(), Some(5.0));
        assert_eq!(info.output_cost_per_mtok(), Some(25.0));
        assert_eq!(info.cache_input_cost_per_mtok(), Some(0.5));
        assert_eq!(info.estimated_output_tps(), Some(25.0));
        assert!(!info.aliases().is_empty());
        assert!(!info.is_default());
    }

    #[test]
    fn all_catalog_providers_are_valid() {
        for model in Catalog::builtin().list(None) {
            // provider() just returns the Provider enum, no parsing needed
            let _ = model.provider();
        }
    }
}
