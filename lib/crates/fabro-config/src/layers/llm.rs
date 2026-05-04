//! Sparse `[llm]` settings layer: provider and model catalog data.

use std::collections::BTreeMap;

use serde::de::Error as _;
use serde::{Deserialize, Serialize};

use super::maps::MergeMap;

/// Deserialize `knowledge_cutoff` from either a TOML date or a string.
///
/// When TOML source contains an unquoted `2025-01-01`, the `toml` crate
/// intermediate `Value` representation stores it as a `Datetime`.
/// When it's quoted `"2025-01-01"`, it's a string. We accept both.
fn deserialize_knowledge_cutoff<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Deserialize as a generic TOML value first, then coerce to string.
    let opt: Option<toml::Value> = Option::deserialize(deserializer)?;
    match opt {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(s)),
        Some(toml::Value::Datetime(dt)) => Ok(Some(dt.to_string())),
        Some(other) => Err(D::Error::custom(format!(
            "expected a date string or TOML date for knowledge_cutoff, got {other}"
        ))),
    }
}

/// Top-level `[llm]` settings layer.
///
/// This only contains `providers` and `models` subtrees.
/// Legacy keys like `provider` or `model` at `[llm]` level should be caught
/// by the parse-time migration hint, not parsed here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct LlmLayer {
    /// `[llm.providers.<id>]` — merge-by-key across layers.
    #[serde(default, skip_serializing_if = "MergeMap::is_empty")]
    pub providers: MergeMap<ProviderSettingsLayer>,
    /// `[llm.models.<id>]` — merge-by-key across layers.
    #[serde(default, skip_serializing_if = "MergeMap::is_empty")]
    pub models:    MergeMap<ModelSettingsLayer>,
}

/// `[llm.providers.<id>]` — a single provider's settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ProviderSettingsLayer {
    /// Human-readable display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Adapter key (e.g. "anthropic", "openai", "openai_compatible").
    /// Validated against the adapter registry at catalog build time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter:      Option<String>,
    /// Base URL for API requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url:     Option<String>,
    /// Ordered credential references. Replaces as whole array across layers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credentials:  Vec<CredentialRef>,
    /// Priority for default provider selection. Higher wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority:     Option<i32>,
    /// Whether this provider is available for runtime selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled:      Option<bool>,
    /// Alternative names for this provider.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases:      Vec<String>,
}

/// `[llm.models.<id>]` — a single model's settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ModelSettingsLayer {
    /// Provider ID this model belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider:             Option<String>,
    /// The model identifier sent to the provider API.
    /// When omitted, defaults to the catalog model ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_id:               Option<String>,
    /// Human-readable display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name:         Option<String>,
    /// Model family (e.g. "claude-4", "gpt-5").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family:               Option<String>,
    /// Knowledge cutoff date (YYYY-MM-DD string).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_knowledge_cutoff"
    )]
    pub knowledge_cutoff:     Option<String>,
    /// Whether this is the default model for its provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default:              Option<bool>,
    /// Whether this model is available for selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled:              Option<bool>,
    /// Alternative names for this model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases:              Vec<String>,
    /// Estimated output tokens per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_output_tps: Option<f64>,
    /// Model limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits:               Option<ModelLimitsLayer>,
    /// Model feature flags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features:             Option<ModelFeaturesLayer>,
    /// Base cost rates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub costs:                Option<ModelCostTableLayer>,
    /// Supported control values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controls:             Option<ModelControlsLayer>,
}

/// Model context window and output limits.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelLimitsLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output:     Option<i64>,
}

/// Model feature flags.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFeaturesLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools:     Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision:    Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort:    Option<bool>,
}

/// Cost rates in USD per million tokens.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostRatesLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cost_per_mtok:       Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_cost_per_mtok:      Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_input_cost_per_mtok: Option<f64>,
}

/// Model cost table: base rates plus optional per-speed overrides.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCostTableLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cost_per_mtok:       Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_cost_per_mtok:      Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_input_cost_per_mtok: Option<f64>,
    /// Per-speed cost overrides. Keys are speed names (e.g. "fast").
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub speed:                     BTreeMap<String, CostRatesLayer>,
}

/// Model control allow-lists.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelControlsLayer {
    /// Allowed reasoning effort values (e.g. `["low", "medium", "high"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_effort: Vec<String>,
    /// Additional speed values beyond standard (e.g. `["fast"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub speed:            Vec<String>,
}

/// A typed credential reference. Only `credential:<id>` and `env:<NAME>`
/// are valid. Literal secrets fail deserialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialRef {
    /// `credential:<id>` — read from fabro-vault.
    Credential(String),
    /// `env:<NAME>` — read from process environment, then vault fallback.
    Env(String),
}

impl std::fmt::Display for CredentialRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Credential(id) => write!(f, "credential:{id}"),
            Self::Env(name) => write!(f, "env:{name}"),
        }
    }
}

impl Serialize for CredentialRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for CredentialRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if let Some(id) = raw.strip_prefix("credential:") {
            if id.is_empty() {
                return Err(D::Error::custom("credential: ref must have a non-empty ID"));
            }
            return Ok(Self::Credential(id.to_string()));
        }
        if let Some(name) = raw.strip_prefix("env:") {
            if name.is_empty() {
                return Err(D::Error::custom("env: ref must have a non-empty name"));
            }
            return Ok(Self::Env(name.to_string()));
        }
        Err(D::Error::custom(format!(
            "invalid credential reference '{raw}': must start with 'credential:' or 'env:'"
        )))
    }
}
