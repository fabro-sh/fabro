use serde::{Deserialize, Serialize};

use crate::ids::ProviderId;

// --- 2.9 Model ---

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ReasoningEffortFeature {
    Levels,
    /// Effort levels are supported, and thinking is natively always-on
    /// adaptive at the endpoint; a manual thinking on/off toggle is not
    /// accepted.
    AlwaysAdaptive,
    #[default]
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelLimits {
    pub context_window: i64,
    pub max_output:     Option<i64>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelFeatures {
    pub tools:            bool,
    pub vision:           bool,
    pub reasoning:        bool,
    /// Whether this model endpoint supports a native reasoning-effort
    /// parameter. User-facing allowed effort values live in catalog controls.
    #[serde(default)]
    pub reasoning_effort: ReasoningEffortFeature,
    /// Whether this model endpoint supports prompt caching annotations.
    #[serde(default)]
    pub prompt_cache:     bool,
    /// Whether the model endpoint accepts classic sampling parameters
    /// (`temperature`, `top_p`). Models with always-on adaptive behavior
    /// reject them.
    #[serde(default = "default_true")]
    pub sampling_params:  bool,
}

impl ModelFeatures {
    /// Whether the model endpoint accepts a native reasoning-effort level.
    #[must_use]
    pub fn supports_reasoning_effort(&self) -> bool {
        matches!(
            self.reasoning_effort,
            ReasoningEffortFeature::Levels | ReasoningEffortFeature::AlwaysAdaptive
        )
    }
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
    pub provider:             ProviderId,
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
    #[serde(default)]
    pub small_default:        bool,
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

    pub fn provider(&self) -> &ProviderId {
        &self.provider
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

    pub fn supports_reasoning_effort(&self) -> bool {
        self.features.supports_reasoning_effort()
    }

    pub fn supports_prompt_cache(&self) -> bool {
        self.features.prompt_cache
    }

    pub fn supports_sampling_params(&self) -> bool {
        self.features.sampling_params
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

    pub fn is_small_default(&self) -> bool {
        self.small_default
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ProviderId;

    #[test]
    fn reasoning_effort_feature_always_adaptive_round_trips() {
        let parsed: ReasoningEffortFeature =
            serde_json::from_value(serde_json::json!("always_adaptive")).unwrap();
        assert_eq!(parsed, ReasoningEffortFeature::AlwaysAdaptive);
        assert_eq!(
            serde_json::to_value(parsed).unwrap(),
            serde_json::json!("always_adaptive")
        );
        assert_eq!(parsed.to_string(), "always_adaptive");
        assert_eq!(
            "always_adaptive".parse::<ReasoningEffortFeature>().unwrap(),
            parsed
        );
    }

    #[test]
    fn inherent_methods_return_correct_values() {
        let info = Model {
            id:                   "model-id".to_string(),
            provider:             ProviderId::new("provider-id"),
            family:               "family".to_string(),
            display_name:         "Display Name".to_string(),
            limits:               ModelLimits {
                context_window: 123_456,
                max_output:     Some(7_890),
            },
            training:             Some("training".to_string()),
            knowledge_cutoff:     Some("knowledge-cutoff".to_string()),
            features:             ModelFeatures {
                tools:            true,
                vision:           true,
                reasoning:        true,
                reasoning_effort: ReasoningEffortFeature::Levels,
                prompt_cache:     true,
                sampling_params:  true,
            },
            costs:                ModelCosts {
                input_cost_per_mtok:       Some(1.0),
                output_cost_per_mtok:      Some(2.0),
                cache_input_cost_per_mtok: Some(0.1),
            },
            estimated_output_tps: Some(42.0),
            aliases:              vec!["alias".to_string()],
            default:              true,
            small_default:        true,
            configured:           false,
        };

        assert_eq!(info.id(), "model-id");
        assert_eq!(info.provider(), &ProviderId::new("provider-id"));
        assert_eq!(info.family(), "family");
        assert_eq!(info.display_name(), "Display Name");
        assert_eq!(info.context_window(), 123_456);
        assert_eq!(info.max_output(), Some(7_890));
        assert!(info.supports_tools());
        assert!(info.supports_vision());
        assert!(info.supports_reasoning());
        assert!(info.supports_reasoning_effort());
        assert!(info.supports_prompt_cache());
        assert!(info.supports_sampling_params());
        assert_eq!(info.training(), Some("training"));
        assert_eq!(info.knowledge_cutoff(), Some("knowledge-cutoff"));
        assert_eq!(info.input_cost_per_mtok(), Some(1.0));
        assert_eq!(info.output_cost_per_mtok(), Some(2.0));
        assert_eq!(info.cache_input_cost_per_mtok(), Some(0.1));
        assert_eq!(info.estimated_output_tps(), Some(42.0));
        assert_eq!(info.aliases(), &["alias".to_string()]);
        assert!(info.is_default());
        assert!(info.is_small_default());
    }
}
