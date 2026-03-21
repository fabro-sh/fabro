use serde::{Deserialize, Serialize};

// --- 2.9 ModelInfo ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelLimits {
    pub context_window: i64,
    pub max_output: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelFeatures {
    pub tools: bool,
    pub vision: bool,
    pub reasoning: bool,
    /// Whether the model supports the `reasoning_effort` / `effort` parameter
    /// directly (e.g. Anthropic `output_config.effort`, OpenAI `reasoning.effort`).
    /// Models with `reasoning=true` but `effort=false` (e.g. claude-sonnet-4-5)
    /// need the older `thinking` API with `budget_tokens` instead.
    #[serde(default)]
    pub effort: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCosts {
    pub input_cost_per_mtok: Option<f64>,
    pub output_cost_per_mtok: Option<f64>,
    pub cache_input_cost_per_mtok: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    pub family: String,
    pub display_name: String,
    pub limits: ModelLimits,
    pub training: Option<String>,
    pub features: ModelFeatures,
    pub costs: ModelCosts,
    pub estimated_output_tps: Option<f64>,
    pub aliases: Vec<String>,
    #[serde(default)]
    pub default: bool,
}
