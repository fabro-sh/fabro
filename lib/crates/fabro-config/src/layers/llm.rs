//! `[llm]` settings layer.
//!
//! Holds the trusted, mergeable LLM provider/model catalog data:
//!
//! ```toml
//! [llm.providers.kimi]
//! display_name = "Kimi"
//! adapter = "openai_compatible"
//! base_url = "https://api.moonshot.ai/v1"
//! credentials = ["credential:kimi", "env:KIMI_API_KEY"]
//! priority = 60
//! enabled = true
//! aliases = ["moonshot"]
//!
//! [llm.models."kimi-k2.5"]
//! provider = "kimi"
//! ...
//! ```
//!
//! Per-provider and per-model entries field-merge across layers (default →
//! user → server → project → workflow/run). Inner arrays such as
//! `credentials`, `aliases`, `controls.reasoning_effort`, and
//! `controls.speed` replace as whole arrays.
//!
//! Adapter keys (`adapter = "..."`) are parsed as plain strings here.
//! Resolution against the static adapter registry happens in `fabro-model`
//! when the resolved [`Catalog`](fabro_model::Catalog) is built.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Deserializer, Serialize};

use super::maps::MergeMap;

/// Top-level `[llm]` settings layer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct LlmLayer {
    /// Provider definitions keyed by provider ID.
    #[serde(default, skip_serializing_if = "MergeMap::is_empty")]
    pub providers: MergeMap<ProviderSettings>,
    /// Model definitions keyed by canonical model ID.
    #[serde(default, skip_serializing_if = "MergeMap::is_empty")]
    pub models:    MergeMap<ModelSettings>,
}

/// One entry in `[llm.providers.<id>]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ProviderSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Adapter registry key (e.g. `"openai_compatible"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter:      Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url:     Option<String>,
    /// Ordered list of credential references — first successful wins. Each
    /// entry must be a typed `CredentialRef` (`credential:<id>` or
    /// `env:<NAME>`); literal secret strings fail deserialization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials:  Option<Vec<CredentialRef>>,
    /// Higher wins; missing → `0`; ties broken by canonical provider ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority:     Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled:      Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases:      Option<Vec<String>>,
}

/// One entry in `[llm.models.<id>]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ModelSettings {
    /// Provider ID this model belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider:             Option<String>,
    /// Identifier sent to the provider API. Defaults to the catalog model ID
    /// when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_id:               Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name:         Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family:               Option<String>,
    /// Knowledge cutoff as an exact `YYYY-MM-DD` date. Lower-precision labels
    /// (e.g. `May 2025`) migrate to the first of the month (`2025-05-01`);
    /// presentation can render lower precision.
    #[serde(
        default,
        deserialize_with = "deserialize_knowledge_cutoff",
        skip_serializing_if = "Option::is_none"
    )]
    pub knowledge_cutoff:     Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default:              Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled:              Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases:              Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_output_tps: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits:               Option<ModelLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features:             Option<ModelFeatures>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controls:             Option<ModelControls>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub costs:                Option<ModelCostTable>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ModelLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output:     Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ModelFeatures {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools:     Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision:    Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort:    Option<bool>,
}

/// User-facing allow-list for native control values Fabro accepts on this
/// model. Whole-array replacement on merge.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ModelControls {
    /// Allowed reasoning-effort values. Strings (e.g. `"low"`, `"high"`,
    /// `"xhigh"`) — validated as `ReasoningEffort` at catalog build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<Vec<String>>,
    /// Additional speeds beyond `Speed::Standard`. Strings — validated as
    /// `Speed` at catalog build. `Speed::Standard` is implicit and must not
    /// appear here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed:            Option<Vec<String>>,
}

/// Pricing table. Base [`CostRates`] always apply; per-speed overrides
/// substitute when the request specifies a non-standard speed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct ModelCostTable {
    #[serde(flatten)]
    pub base:  CostRates,
    /// Per-speed cost overrides (e.g. `costs.speed.fast = { ... }`). Keys
    /// must reference a speed declared in `controls.speed`. `standard` is
    /// not a valid override key — base rates serve standard speed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<BTreeMap<String, CostRates>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, fabro_macros::Combine)]
#[serde(deny_unknown_fields)]
pub struct CostRates {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cost_per_mtok:       Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_cost_per_mtok:      Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_input_cost_per_mtok: Option<f64>,
}

/// Accept either a TOML local-date (`2025-01-01` → `Datetime`) or a
/// `YYYY-MM-DD` string for `knowledge_cutoff`. JSON has no native date
/// literal; settings authors use the bare TOML date form, but JSON loaders
/// (e.g. defaults bundled as JSON) supply a string.
fn deserialize_knowledge_cutoff<'de, D>(deserializer: D) -> Result<Option<NaiveDate>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    use toml::value::Datetime;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Toml(Datetime),
        Str(String),
    }

    let value = Option::<Either>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(Either::Str(s)) => NaiveDate::parse_from_str(&s, "%Y-%m-%d")
            .map(Some)
            .map_err(D::Error::custom),
        Some(Either::Toml(dt)) => {
            let date = dt
                .date
                .ok_or_else(|| D::Error::custom("knowledge_cutoff requires a date component"))?;
            NaiveDate::from_ymd_opt(date.year.into(), date.month.into(), date.day.into())
                .ok_or_else(|| D::Error::custom("knowledge_cutoff is not a valid calendar date"))
                .map(Some)
        }
    }
}

// ---------------------------------------------------------------------------
// CredentialRef — typed credential reference
// ---------------------------------------------------------------------------

/// A typed credential reference. Literal secret strings are rejected at
/// deserialization so settings never carry a successful "secret string"
/// representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(into = "String", try_from = "String")]
pub enum CredentialRef {
    /// Structured credential stored in `fabro-vault` keyed by `<id>`.
    Credential(String),
    /// Process environment variable `<NAME>`. Falls back to a raw vault
    /// secret with the same name when the env var is unset.
    Env(String),
}

impl CredentialRef {
    /// The string form, e.g. `"credential:openai_codex"` or
    /// `"env:KIMI_API_KEY"`. Stable for serialization round-trips.
    #[must_use]
    pub fn as_serialized(&self) -> String {
        match self {
            Self::Credential(id) => format!("credential:{id}"),
            Self::Env(name) => format!("env:{name}"),
        }
    }
}

impl std::fmt::Display for CredentialRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Display deliberately writes only the typed reference form, never
        // any resolved secret value. Env names and credential IDs are not
        // themselves secret.
        f.write_str(&self.as_serialized())
    }
}

impl From<CredentialRef> for String {
    fn from(value: CredentialRef) -> Self {
        value.as_serialized()
    }
}

/// Error returned when a credential string is neither `credential:<id>` nor
/// `env:<NAME>`. Literal secret strings always fall into this branch and
/// fail deserialization — by design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialRefParseError {
    /// Reason the input was rejected. Never echoes the input itself, since
    /// it could be a literal secret.
    message: String,
}

impl std::fmt::Display for CredentialRefParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CredentialRefParseError {}

impl std::str::FromStr for CredentialRef {
    type Err = CredentialRefParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(id) = s.strip_prefix("credential:") {
            if id.is_empty() {
                return Err(CredentialRefParseError {
                    message: "credential reference is missing an ID after `credential:`"
                        .to_string(),
                });
            }
            return Ok(Self::Credential(id.to_string()));
        }
        if let Some(name) = s.strip_prefix("env:") {
            if name.is_empty() {
                return Err(CredentialRefParseError {
                    message: "credential reference is missing a name after `env:`".to_string(),
                });
            }
            return Ok(Self::Env(name.to_string()));
        }
        Err(CredentialRefParseError {
            message: "credential reference must be `credential:<id>` or `env:<NAME>`; literal \
                      secret strings are rejected"
                .to_string(),
        })
    }
}

impl TryFrom<String> for CredentialRef {
    type Error = CredentialRefParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl<'de> Deserialize<'de> for CredentialRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(D::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Combine glue for primitives that don't already have it.
// ---------------------------------------------------------------------------

use super::combine::Combine;

// `Option<i32>`, `Option<i64>`, etc. already have `Combine` impls in
// `combine.rs`. Only add the impls that are unique to the LLM layer types.

impl Combine for Option<f64> {
    fn combine(self, other: Self) -> Self {
        self.or(other)
    }
}

impl Combine for Option<NaiveDate> {
    fn combine(self, other: Self) -> Self {
        self.or(other)
    }
}

impl Combine for Option<Vec<CredentialRef>> {
    fn combine(self, other: Self) -> Self {
        self.or(other)
    }
}

impl Combine for Option<BTreeMap<String, CostRates>> {
    fn combine(self, other: Self) -> Self {
        self.or(other)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    // ---- CredentialRef ----------------------------------------------------

    #[test]
    fn credential_ref_parses_credential_form() {
        let r = CredentialRef::from_str("credential:openai_codex").unwrap();
        assert_eq!(r, CredentialRef::Credential("openai_codex".to_string()));
    }

    #[test]
    fn credential_ref_parses_env_form() {
        let r = CredentialRef::from_str("env:KIMI_API_KEY").unwrap();
        assert_eq!(r, CredentialRef::Env("KIMI_API_KEY".to_string()));
    }

    #[test]
    fn credential_ref_rejects_literal_secret() {
        // A literal API key contains no `credential:` or `env:` prefix.
        let err = CredentialRef::from_str("sk-ant-1234").unwrap_err();
        assert!(err.to_string().contains("must be"));
        assert!(
            !err.to_string().contains("sk-ant-1234"),
            "error must not echo the literal secret string back to the user",
        );
    }

    #[test]
    fn credential_ref_rejects_empty_credential_id() {
        let err = CredentialRef::from_str("credential:").unwrap_err();
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn credential_ref_rejects_empty_env_name() {
        let err = CredentialRef::from_str("env:").unwrap_err();
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn credential_ref_round_trips_through_string() {
        let r = CredentialRef::Credential("kimi".to_string());
        assert_eq!(r.to_string(), "credential:kimi");
        let back: CredentialRef = r.to_string().parse().unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn credential_ref_serializes_as_string_in_toml() {
        let r = CredentialRef::Env("KIMI_API_KEY".to_string());
        let s = toml::Value::try_from(&r).unwrap();
        assert_eq!(s.as_str(), Some("env:KIMI_API_KEY"));
    }

    #[test]
    fn credential_ref_deserializes_from_toml_string() {
        let parsed: CredentialRef = toml::from_str(r#"v = "credential:foo""#)
            .map(|v: toml::Value| {
                v.as_table()
                    .unwrap()
                    .get("v")
                    .unwrap()
                    .clone()
                    .try_into()
                    .unwrap()
            })
            .unwrap();
        assert_eq!(parsed, CredentialRef::Credential("foo".to_string()));
    }

    #[test]
    fn credential_ref_in_array_rejects_literal_secret() {
        // serde rejects literal secrets when parsed inside an array of
        // CredentialRef. The error bubbles up as a TOML deserialization
        // failure.
        #[derive(Deserialize)]
        #[expect(
            dead_code,
            reason = "field exists only to drive the deserializer; we assert on the parse error"
        )]
        struct Wrap {
            v: Vec<CredentialRef>,
        }
        let err: Result<Wrap, _> = toml::from_str(r#"v = ["sk-literal-secret"]"#);
        assert!(err.is_err(), "literal secret strings must fail to parse");
    }

    // ---- LlmLayer parsing -------------------------------------------------

    #[test]
    fn parses_minimal_provider_entry() {
        let toml = r#"
[providers.kimi]
display_name = "Kimi"
adapter = "openai_compatible"
base_url = "https://api.moonshot.ai/v1"
credentials = ["credential:kimi", "env:KIMI_API_KEY"]
priority = 60
enabled = true
aliases = ["moonshot"]
"#;
        let layer: LlmLayer = toml::from_str(toml).unwrap();
        let kimi = layer.providers.get("kimi").unwrap();
        assert_eq!(kimi.display_name.as_deref(), Some("Kimi"));
        assert_eq!(kimi.adapter.as_deref(), Some("openai_compatible"));
        assert_eq!(kimi.base_url.as_deref(), Some("https://api.moonshot.ai/v1"));
        assert_eq!(kimi.priority, Some(60));
        assert_eq!(kimi.enabled, Some(true));
        assert_eq!(kimi.aliases.as_deref(), Some(&["moonshot".to_string()][..]));
        assert_eq!(kimi.credentials.as_ref().unwrap(), &vec![
            CredentialRef::Credential("kimi".to_string()),
            CredentialRef::Env("KIMI_API_KEY".to_string()),
        ]);
    }

    #[test]
    fn parses_full_model_entry() {
        let toml = r#"
[models."kimi-k2.5"]
provider = "kimi"
api_id = "kimi-k2.5"
display_name = "Kimi K2.5"
family = "kimi"
knowledge_cutoff = 2025-01-01
default = true
enabled = true
aliases = ["kimi"]
estimated_output_tps = 50

[models."kimi-k2.5".limits]
context_window = 262144
max_output = 32768

[models."kimi-k2.5".features]
tools = true
vision = false
reasoning = true
effort = false

[models."kimi-k2.5".costs]
input_cost_per_mtok = 0.60
output_cost_per_mtok = 2.50
cache_input_cost_per_mtok = 0.15
"#;
        let layer: LlmLayer = toml::from_str(toml).unwrap();
        let m = layer.models.get("kimi-k2.5").unwrap();
        assert_eq!(m.provider.as_deref(), Some("kimi"));
        assert_eq!(m.api_id.as_deref(), Some("kimi-k2.5"));
        assert_eq!(m.display_name.as_deref(), Some("Kimi K2.5"));
        assert_eq!(m.family.as_deref(), Some("kimi"));
        assert_eq!(
            m.knowledge_cutoff,
            Some(NaiveDate::from_ymd_opt(2025, 1, 1).unwrap())
        );
        assert_eq!(m.default, Some(true));
        assert_eq!(m.enabled, Some(true));
        assert_eq!(m.aliases.as_deref(), Some(&["kimi".to_string()][..]));
        assert_eq!(m.estimated_output_tps, Some(50.0));

        let limits = m.limits.as_ref().unwrap();
        assert_eq!(limits.context_window, Some(262_144));
        assert_eq!(limits.max_output, Some(32_768));

        let features = m.features.as_ref().unwrap();
        assert_eq!(features.tools, Some(true));
        assert_eq!(features.vision, Some(false));
        assert_eq!(features.reasoning, Some(true));
        assert_eq!(features.effort, Some(false));

        let costs = m.costs.as_ref().unwrap();
        assert_eq!(costs.base.input_cost_per_mtok, Some(0.60));
        assert_eq!(costs.base.output_cost_per_mtok, Some(2.50));
        assert_eq!(costs.base.cache_input_cost_per_mtok, Some(0.15));
        assert!(costs.speed.is_none());
    }

    #[test]
    fn parses_controls_and_per_speed_costs() {
        let toml = r#"
[models."claude-opus-4-6".controls]
reasoning_effort = ["low", "medium", "high"]
speed = ["fast"]

[models."claude-opus-4-6".costs.speed.fast]
input_cost_per_mtok = 90.0
output_cost_per_mtok = 450.0
cache_input_cost_per_mtok = 9.0
"#;
        let layer: LlmLayer = toml::from_str(toml).unwrap();
        let m = layer.models.get("claude-opus-4-6").unwrap();

        let controls = m.controls.as_ref().unwrap();
        assert_eq!(
            controls.reasoning_effort.as_deref(),
            Some(&["low".to_string(), "medium".to_string(), "high".to_string()][..])
        );
        assert_eq!(controls.speed.as_deref(), Some(&["fast".to_string()][..]));

        let costs = m.costs.as_ref().unwrap();
        let fast = costs.speed.as_ref().unwrap().get("fast").unwrap();
        assert_eq!(fast.input_cost_per_mtok, Some(90.0));
        assert_eq!(fast.output_cost_per_mtok, Some(450.0));
        assert_eq!(fast.cache_input_cost_per_mtok, Some(9.0));
    }

    #[test]
    fn rejects_unknown_provider_field() {
        let toml = r#"
[providers.kimi]
adapter = "openai_compatible"
unknown_field = true
"#;
        let err = toml::from_str::<LlmLayer>(toml).unwrap_err();
        assert!(err.to_string().contains("unknown_field"));
    }

    #[test]
    fn rejects_unknown_model_field() {
        let toml = r#"
[models.foo]
provider = "x"
mystery = 1
"#;
        let err = toml::from_str::<LlmLayer>(toml).unwrap_err();
        assert!(err.to_string().contains("mystery"));
    }

    // ---- Combine / merge --------------------------------------------------

    #[test]
    fn provider_field_merge_keeps_self_values_and_fills_holes() {
        let high = ProviderSettings {
            adapter: Some("openai_compatible".to_string()),
            base_url: Some("https://override.example".to_string()),
            ..ProviderSettings::default()
        };
        let low = ProviderSettings {
            adapter: Some("anthropic".to_string()),
            base_url: Some("https://defaults.example".to_string()),
            display_name: Some("Default".to_string()),
            priority: Some(10),
            ..ProviderSettings::default()
        };
        let merged = high.combine(low);
        assert_eq!(merged.adapter.as_deref(), Some("openai_compatible"));
        assert_eq!(merged.base_url.as_deref(), Some("https://override.example"));
        assert_eq!(merged.display_name.as_deref(), Some("Default"));
        assert_eq!(merged.priority, Some(10));
    }

    #[test]
    fn provider_credentials_array_replaces_wholesale() {
        // Higher layer redeclares credentials → low layer's list is dropped
        // entirely (whole-array replacement).
        let high = ProviderSettings {
            credentials: Some(vec![CredentialRef::Env("FOO".to_string())]),
            ..ProviderSettings::default()
        };
        let low = ProviderSettings {
            credentials: Some(vec![
                CredentialRef::Credential("bar".to_string()),
                CredentialRef::Env("BAZ".to_string()),
            ]),
            ..ProviderSettings::default()
        };
        let merged = high.combine(low);
        assert_eq!(merged.credentials.unwrap(), vec![CredentialRef::Env(
            "FOO".to_string()
        )]);
    }

    #[test]
    fn provider_credentials_inherits_when_unset_in_higher_layer() {
        let high = ProviderSettings::default();
        let low = ProviderSettings {
            credentials: Some(vec![CredentialRef::Env("FOO".to_string())]),
            ..ProviderSettings::default()
        };
        let merged = high.combine(low);
        assert_eq!(merged.credentials.unwrap(), vec![CredentialRef::Env(
            "FOO".to_string()
        )]);
    }

    #[test]
    fn merge_map_field_merges_per_provider_id() {
        let mut high_map: std::collections::HashMap<String, ProviderSettings> =
            std::collections::HashMap::new();
        high_map.insert("kimi".to_string(), ProviderSettings {
            base_url: Some("https://override".to_string()),
            ..ProviderSettings::default()
        });
        let high: MergeMap<ProviderSettings> = MergeMap::from(high_map);

        let mut low_map: std::collections::HashMap<String, ProviderSettings> =
            std::collections::HashMap::new();
        low_map.insert("kimi".to_string(), ProviderSettings {
            adapter: Some("openai_compatible".to_string()),
            base_url: Some("https://defaults".to_string()),
            ..ProviderSettings::default()
        });
        let low: MergeMap<ProviderSettings> = MergeMap::from(low_map);

        let merged = high.combine(low);
        let kimi = merged.get("kimi").unwrap();
        assert_eq!(kimi.adapter.as_deref(), Some("openai_compatible"));
        assert_eq!(kimi.base_url.as_deref(), Some("https://override"));
    }

    #[test]
    fn model_controls_replace_wholesale() {
        // Whole-array replacement: high layer's `reasoning_effort` shadows
        // the low layer's list completely.
        let high = ModelControls {
            reasoning_effort: Some(vec!["high".to_string()]),
            ..ModelControls::default()
        };
        let low = ModelControls {
            reasoning_effort: Some(vec!["low".to_string(), "high".to_string()]),
            speed:            Some(vec!["fast".to_string()]),
        };
        let merged = high.combine(low);
        assert_eq!(
            merged.reasoning_effort.as_deref(),
            Some(&["high".to_string()][..])
        );
        assert_eq!(merged.speed.as_deref(), Some(&["fast".to_string()][..]));
    }
}
