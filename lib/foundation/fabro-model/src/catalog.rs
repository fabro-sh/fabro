use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::str::FromStr;
use std::sync::LazyLock;

use rust_embed::RustEmbed;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use strum::VariantArray;
use toml::de::Error as TomlDeError;
use tracing::warn;

use crate::Speed;
use crate::adapter::{AdapterKind, AgentProfileKind};
use crate::codec::CodecKind;
use crate::ids::{ModelId, ProviderId};
use crate::provider::Provider;
use crate::reasoning::ReasoningEffort;
use crate::types::{
    Model, ModelControls, ModelCosts, ModelFeatures, ModelLimits, ReasoningEffortFeature,
};

#[derive(RustEmbed)]
#[folder = "src/catalog/providers"]
struct BuiltinCatalogToml;

/// TOML shape used by the model catalog builder.
///
/// This deliberately lives in `fabro-model` instead of reusing
/// `fabro-config::LlmLayer`: `fabro-config` depends on `fabro-types`, and
/// `fabro-types` depends on `fabro-model`, so the catalog cannot depend on
/// `fabro-config` without creating a crate cycle.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmCatalogSettings {
    #[serde(default)]
    pub providers: HashMap<String, ProviderCatalogSettings>,
    /// Legacy `[models."<id>"]` input. Canonical settings place model rows
    /// under their provider; this map is normalized before layers merge.
    #[serde(default)]
    pub models:    HashMap<String, ModelCatalogSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCatalogSettings {
    #[serde(default)]
    pub display_name:   Option<String>,
    #[serde(default)]
    pub adapter:        Option<String>,
    /// Wire dialect for this provider's routes. Defaults to the adapter's
    /// codec; only the default pairing is accepted today.
    #[serde(default)]
    pub codec:          Option<CodecKind>,
    #[serde(default)]
    pub agent_profile:  Option<AgentProfileKind>,
    #[serde(default)]
    pub auth:           Option<ProviderAuthConfig>,
    #[serde(default)]
    pub billing_policy: Option<BillingPolicy>,
    #[serde(default)]
    pub api_key_url:    Option<String>,
    #[serde(default)]
    pub base_url:       Option<String>,
    /// Unresolved interpolation source strings (literal text or
    /// `{{ secrets.NAME }}` tokens), resolved at the credential boundary in
    /// `fabro-auth`.
    #[serde(default)]
    pub extra_headers:  Option<HashMap<String, String>>,
    #[serde(default)]
    pub priority:       Option<i32>,
    #[serde(default)]
    pub enabled:        Option<bool>,
    #[serde(default)]
    pub aliases:        Option<Vec<String>>,
    /// Model declarations keyed by Fabro's canonical model slug.
    #[serde(default)]
    pub models:         HashMap<String, ModelCatalogSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCatalogSettings {
    /// Provider used only by the temporary legacy top-level `[models]`
    /// compatibility shape. Canonical provider-scoped rows leave this unset.
    #[serde(default)]
    pub provider:             Option<String>,
    #[serde(default)]
    pub api_id:               Option<String>,
    /// Wire dialect for this model's route, overriding the provider's codec
    /// (the multiplexer case). Only the adapter's default pairing is
    /// accepted today.
    #[serde(default)]
    pub codec:                Option<CodecKind>,
    /// Billing family for this model, overriding the provider's policy
    /// (e.g. Anthropic cache billing for a Claude model served through an
    /// aggregator whose other models bill OpenAI-style).
    #[serde(default)]
    pub billing_policy:       Option<BillingPolicy>,
    #[serde(default)]
    pub agent_profile:        Option<AgentProfileKind>,
    #[serde(default)]
    pub display_name:         Option<String>,
    #[serde(default)]
    pub family:               Option<String>,
    #[serde(default)]
    pub training:             Option<String>,
    #[serde(default, deserialize_with = "deserialize_knowledge_cutoff")]
    pub knowledge_cutoff:     Option<String>,
    #[serde(default)]
    pub default:              Option<bool>,
    #[serde(default)]
    pub small_default:        Option<bool>,
    #[serde(default)]
    pub probe:                Option<bool>,
    #[serde(default)]
    pub enabled:              Option<bool>,
    #[serde(default)]
    pub aliases:              Option<Vec<String>>,
    #[serde(default)]
    pub estimated_output_tps: Option<f64>,
    #[serde(default)]
    pub limits:               Option<SettingsModelLimits>,
    #[serde(default)]
    pub features:             Option<SettingsModelFeatures>,
    #[serde(default)]
    pub controls:             Option<SettingsModelControls>,
    #[serde(default)]
    pub costs:                Option<SettingsModelCostTable>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsModelLimits {
    #[serde(default)]
    pub context_window: Option<i64>,
    #[serde(default)]
    pub max_output:     Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsModelFeatures {
    #[serde(default)]
    pub tools:                     Option<bool>,
    #[serde(default)]
    pub vision:                    Option<bool>,
    #[serde(default)]
    pub reasoning:                 Option<bool>,
    /// Whether requests reason when no effort control is supplied. When
    /// omitted, effort-capable models default to `true` and other models to
    /// `false`.
    #[serde(default)]
    pub reasoning_by_default:      Option<bool>,
    #[serde(default)]
    pub reasoning_effort:          Option<ReasoningEffortFeature>,
    #[serde(default)]
    pub prompt_cache:              Option<bool>,
    #[serde(default)]
    pub cache_control_breakpoints: Option<bool>,
    #[serde(default)]
    pub sampling_params:           Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsModelControls {
    #[serde(default)]
    pub reasoning_effort: Option<Vec<String>>,
    #[serde(default)]
    pub speed:            Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsModelCostTable {
    #[serde(flatten)]
    pub base:  CostRates,
    #[serde(default)]
    pub speed: Option<BTreeMap<String, CostRates>>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostRates {
    #[serde(default)]
    pub input_cost_per_mtok:       Option<f64>,
    #[serde(default)]
    pub output_cost_per_mtok:      Option<f64>,
    #[serde(default)]
    pub cache_input_cost_per_mtok: Option<f64>,
}

/// Where a provider's credential comes from.
///
/// `Vault`/`Env` reference a stored secret resolved to an auth header.
/// `AwsSigv4` is an opaque source: the credential comes from the AWS default
/// credential chain and the request is SigV4-signed rather than carrying a
/// static secret. It is only valid on Bedrock providers, which catalog
/// validation enforces before adapter construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum CredentialRef {
    Vault(String),
    Env(String),
    AwsSigv4,
}

impl std::fmt::Display for CredentialRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Vault(name) => write!(f, "vault:{name}"),
            Self::Env(name) => write!(f, "env:{name}"),
            Self::AwsSigv4 => write!(f, "aws_sigv4"),
        }
    }
}

impl From<CredentialRef> for String {
    fn from(value: CredentialRef) -> Self {
        value.to_string()
    }
}

impl FromStr for CredentialRef {
    type Err = CredentialRefParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some(name) = value.strip_prefix("vault:") {
            if name.is_empty() {
                return Err(CredentialRefParseError::EmptyVault);
            }
            return Ok(Self::Vault(name.to_string()));
        }
        if let Some(name) = value.strip_prefix("env:") {
            if name.is_empty() {
                return Err(CredentialRefParseError::EmptyEnv);
            }
            return Ok(Self::Env(name.to_string()));
        }
        if value == "aws_sigv4" {
            return Ok(Self::AwsSigv4);
        }
        Err(CredentialRefParseError::Invalid)
    }
}

impl TryFrom<String> for CredentialRef {
    type Error = CredentialRefParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CredentialRefParseError {
    #[error("credential reference must be `vault:<name>`, `env:<NAME>`, or `aws_sigv4`")]
    Invalid,
    #[error("credential reference is missing a name after `vault:`")]
    EmptyVault,
    #[error("credential reference is missing a name after `env:`")]
    EmptyEnv,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderAuthConfig {
    /// Ordered credential sources; the first that resolves wins. Static secrets
    /// use `env:<NAME>` / `vault:<NAME>`; AWS SigV4 (Bedrock) uses `aws_sigv4`,
    /// which resolves opaquely from the AWS credential chain.
    pub credentials: Vec<CredentialRef>,
    #[serde(default)]
    pub header:      ApiKeyHeaderPolicy,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ApiKeyHeaderPolicy {
    #[default]
    Bearer,
    Custom {
        name: String,
    },
}

impl Serialize for ApiKeyHeaderPolicy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Bearer => serializer.serialize_str("bearer"),
            Self::Custom { name } => {
                use serde::ser::SerializeMap;

                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("custom", name)?;
                map.end()
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ApiKeyHeaderPolicyInput {
    String(String),
    Table(ApiKeyHeaderPolicyTable),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiKeyHeaderPolicyTable {
    custom: String,
}

impl<'de> Deserialize<'de> for ApiKeyHeaderPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error as _;

        match ApiKeyHeaderPolicyInput::deserialize(deserializer)? {
            ApiKeyHeaderPolicyInput::String(value) if value == "bearer" => Ok(Self::Bearer),
            ApiKeyHeaderPolicyInput::String(value) => Err(D::Error::custom(format!(
                "API key header must be `bearer`, got `{value}`"
            ))),
            ApiKeyHeaderPolicyInput::Table(table) => {
                validate_header_name(&table.custom).map_err(D::Error::custom)?;
                Ok(Self::Custom { name: table.custom })
            }
        }
    }
}

fn validate_header_name(name: &str) -> Result<(), &'static str> {
    http::HeaderName::from_bytes(name.as_bytes())
        .map(|_| ())
        .map_err(|_| "custom header name must be a valid HTTP header name")
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum BillingPolicy {
    #[serde(rename = "openai")]
    #[strum(to_string = "openai")]
    OpenAi,
    Anthropic,
    Gemini,
    None,
}

pub fn deserialize_knowledge_cutoff<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error as _;
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
        Some(Either::Str(value)) => Ok(Some(value)),
        Some(Either::Toml(value)) => {
            let date = value
                .date
                .ok_or_else(|| D::Error::custom("knowledge_cutoff requires a date component"))?;
            Ok(Some(format!(
                "{:04}-{:02}-{:02}",
                date.year, date.month, date.day
            )))
        }
    }
}

/// Global singleton catalog parsed from embedded provider TOML files.
static GLOBAL_CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    Catalog::from_builtin_toml().expect("embedded provider TOML files must build a valid catalog")
});

/// A resolved fallback target: provider name + model ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackTarget {
    pub provider: ProviderId,
    pub model:    ModelId,
}

impl FallbackTarget {
    /// Build a target from anything that renders as a provider name and model
    /// ID, so callers holding [`ProviderId`]/[`ModelId`] or bare passthrough
    /// selectors all use one constructor.
    pub fn new(provider: impl std::fmt::Display, model: impl std::fmt::Display) -> Self {
        Self {
            provider: ProviderId::new(provider.to_string()),
            model:    ModelId::new(model.to_string()),
        }
    }
}

impl std::fmt::Display for FallbackTarget {
    /// Renders as `provider:model`, matching the qualified form accepted by
    /// model references.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.provider, self.model)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogProvider {
    pub id:             ProviderId,
    pub display_name:   String,
    pub adapter:        AdapterKind,
    /// Wire dialect driven by this provider's routes; models may override it
    /// via [`CatalogModelSettings::codec`].
    pub codec:          CodecKind,
    pub agent_profile:  AgentProfileKind,
    pub auth:           Option<ProviderAuthConfig>,
    pub billing_policy: BillingPolicy,
    pub api_key_url:    Option<String>,
    pub base_url:       Option<String>,
    /// Unresolved interpolation source strings (literal text or
    /// `{{ secrets.NAME }}` tokens), resolved at the credential boundary in
    /// `fabro-auth`.
    pub extra_headers:  HashMap<String, String>,
    pub priority:       i32,
    pub aliases:        Vec<String>,
}

impl CatalogProvider {
    #[must_use]
    pub fn vault_secret_name(&self) -> Option<&str> {
        self.auth
            .as_ref()?
            .credentials
            .iter()
            .find_map(|credential_ref| match credential_ref {
                CredentialRef::Vault(name) => Some(name.as_str()),
                CredentialRef::Env(_) | CredentialRef::AwsSigv4 => None,
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogModelControls {
    pub reasoning_effort: Vec<ReasoningEffort>,
    pub speed:            Vec<Speed>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogModelSettings {
    pub api_id:               String,
    /// Wire dialect for this model's route (the provider codec unless the
    /// model row overrides it).
    pub codec:                CodecKind,
    /// Billing family for this model (the provider policy unless the model
    /// row overrides it).
    pub billing_policy:       BillingPolicy,
    pub agent_profile:        AgentProfileKind,
    /// Whether the provider route reasons when a request omits an effort
    /// control.
    pub reasoning_by_default: bool,
    pub controls:             CatalogModelControls,
    pub speed_costs:          HashMap<Speed, ModelCosts>,
    probe:                    bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogBuildError {
    #[error("embedded built-in catalog contains no provider TOML files")]
    NoBuiltinProviderFiles,
    #[error("failed to read embedded provider TOML path '{path}' as UTF-8")]
    InvalidBuiltinUtf8 {
        path:   String,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("failed to parse embedded provider TOML '{path}'")]
    InvalidBuiltinToml {
        path:   String,
        #[source]
        source: TomlDeError,
    },
    #[error("embedded provider TOML '{path}' must define exactly one provider row")]
    InvalidBuiltinProviderCount { path: String },
    #[error("embedded provider TOML '{path}' must define provider '{expected}', found '{actual}'")]
    BuiltinProviderIdMismatch {
        path:     String,
        expected: String,
        actual:   String,
    },
    #[error(
        "embedded provider TOML '{path}' contains model '{model}' for provider '{actual}', expected '{expected}'"
    )]
    BuiltinModelProviderMismatch {
        path:     String,
        model:    String,
        expected: String,
        actual:   String,
    },
    #[error("provider '{provider}' is missing required field '{field}'")]
    MissingProviderField {
        provider: ProviderId,
        field:    &'static str,
    },
    #[error("provider '{provider}' uses unknown adapter '{adapter}'")]
    UnknownAdapter {
        provider: ProviderId,
        adapter:  String,
    },
    #[error(
        "provider '{provider}' configures codec '{codec}', but adapter '{adapter}' only supports '{expected}'"
    )]
    UnsupportedProviderCodec {
        provider: ProviderId,
        adapter:  AdapterKind,
        codec:    CodecKind,
        expected: CodecKind,
    },
    #[error(
        "model '{model}' configures codec '{codec}', but adapter '{adapter}' only supports '{expected}'"
    )]
    UnsupportedModelCodec {
        model:    String,
        adapter:  AdapterKind,
        codec:    CodecKind,
        expected: CodecKind,
    },
    #[error("provider '{provider}' API-key auth must declare at least one credential")]
    EmptyApiKeyCredentials { provider: ProviderId },
    #[error(
        "provider '{provider}' uses aws_sigv4 credentials, but adapter '{adapter}' does not support SigV4"
    )]
    UnsupportedAwsSigv4Credential {
        provider: ProviderId,
        adapter:  AdapterKind,
    },
    #[error("provider identifier '{identifier}' is declared by both '{first}' and '{second}'")]
    DuplicateProviderIdentifier {
        identifier: String,
        first:      ProviderId,
        second:     ProviderId,
    },
    #[error("model '{model}' is missing required field '{field}'")]
    MissingModelField { model: String, field: &'static str },
    #[error("model '{model}' references unknown provider '{provider}'")]
    UnknownModelProvider {
        model:    String,
        provider: ProviderId,
    },
    #[error(
        "provider '{provider}' model selector '{selector}' is declared by both '{first}' and '{second}'"
    )]
    DuplicateProviderModelSelector {
        provider: ProviderId,
        selector: String,
        first:    ModelId,
        second:   ModelId,
    },
    #[error(transparent)]
    LegacyModel(#[from] LegacyModelError),
    #[error("provider '{provider}' model '{model}' has an empty api_id")]
    EmptyModelApiId {
        provider: ProviderId,
        model:    ModelId,
    },
    #[error("provider '{provider}' has multiple default models: {models:?}")]
    MultipleProviderDefaults {
        provider: ProviderId,
        models:   Vec<String>,
    },
    #[error("provider '{provider}' has multiple small default models: {models:?}")]
    MultipleProviderSmallDefaults {
        provider: ProviderId,
        models:   Vec<String>,
    },
    #[error("catalog must contain at least one enabled default model")]
    NoDefaultModel,
    #[error("model '{model}' has invalid reasoning_effort '{value}'")]
    InvalidReasoningEffort {
        model:  String,
        value:  String,
        #[source]
        source: strum::ParseError,
    },
    #[error("model '{model}' declares reasoning_effort controls but features.reasoning is false")]
    ReasoningEffortControlsWithoutReasoning { model: String },
    #[error("model '{model}' declares reasoning_effort feature but features.reasoning is false")]
    ReasoningEffortWithoutReasoning { model: String },
    #[error("model '{model}' sets reasoning_by_default but features.reasoning is false")]
    DefaultReasoningWithoutReasoning { model: String },
    #[error(
        "model '{model}' declares cache_control_breakpoints but features.prompt_cache is false"
    )]
    CacheControlBreakpointsWithoutPromptCache { model: String },
    #[error(
        "model '{model}' must declare at least one reasoning_effort when features.reasoning_effort is levels or always_adaptive"
    )]
    EmptyReasoningEffortControls { model: String },
    #[error("model '{model}' has invalid speed '{value}'")]
    InvalidSpeed {
        model:  String,
        value:  String,
        #[source]
        source: strum::ParseError,
    },
    #[error("model '{model}' must not declare standard in controls.speed")]
    StandardSpeedControl { model: String },
    #[error("model '{model}' has costs.speed.{speed} without declaring controls.speed")]
    UndeclaredSpeedCost { model: String, speed: Speed },
}

/// Failure to select one concrete provider/model offering.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelSelectionError {
    #[error("unknown model provider '{provider}'")]
    UnknownProvider { provider: ProviderId },
    #[error("model provider '{provider}' is unavailable")]
    ProviderUnavailable { provider: ProviderId },
    #[error("unknown model selector '{selector}'")]
    UnknownSelector { selector: String },
    #[error("model selector '{selector}' is unknown on provider '{provider}'")]
    UnknownSelectorOnProvider {
        selector: String,
        provider: ProviderId,
    },
    #[error(
        "model selector '{selector}' is known but has no offering on an eligible provider; available providers: {providers:?}"
    )]
    NoEligibleOffering {
        selector:  String,
        providers: Vec<ProviderId>,
    },
    #[error(
        "no default model is available on an eligible provider; providers with defaults: {providers:?}"
    )]
    NoDefaultModel { providers: Vec<ProviderId> },
}

/// One provider/model pair chosen by [`Catalog::resolve_selection`]. The
/// model is the canonical catalog ID when the selector matched an offering,
/// or the caller's selector passed through verbatim when it did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedModel {
    pub provider: ProviderId,
    pub model:    String,
}

/// Typed model catalog backed by a `Vec<Model>`.
///
/// Use [`Catalog::builtin()`] for the embedded settings-backed catalog.
#[derive(Debug)]
pub struct Catalog {
    models:                  Vec<Model>,
    providers:               Vec<CatalogProvider>,
    model_settings:          HashMap<(ProviderId, ModelId), CatalogModelSettings>,
    offering_index:          HashMap<(ProviderId, ModelId), usize>,
    provider_selector_index: HashMap<(ProviderId, String), usize>,
    canonical_candidates:    HashMap<ModelId, Vec<usize>>,
    alias_candidates:        HashMap<String, Vec<usize>>,
    provider_aliases:        HashMap<String, ProviderId>,
    provider_index:          HashMap<ProviderId, usize>,
}

impl Catalog {
    /// Returns a reference to the global built-in catalog (loaded once from
    /// embedded provider TOML files).
    #[must_use]
    pub fn builtin() -> &'static Self {
        &GLOBAL_CATALOG
    }

    pub fn from_settings(settings: &LlmCatalogSettings) -> Result<Self, CatalogBuildError> {
        let settings = normalize_catalog_settings(settings.clone(), None)?;
        let mut providers = build_providers(&settings)?;
        providers.sort_by(provider_order);

        let mut provider_index = HashMap::new();
        for (idx, provider) in providers.iter().enumerate() {
            provider_index.insert(provider.id.clone(), idx);
        }

        let provider_aliases = build_provider_aliases(&providers)?;
        let known_providers: HashSet<&str> =
            settings.providers.keys().map(String::as_str).collect();
        let enabled_providers: HashSet<&str> = providers
            .iter()
            .map(|provider| provider.id.as_str())
            .collect();
        let provider_by_id: HashMap<&str, &CatalogProvider> = providers
            .iter()
            .map(|provider| (provider.id.as_str(), provider))
            .collect();

        let mut models_with_settings = Vec::new();
        let mut model_identifiers = HashMap::<ProviderId, BTreeMap<String, ModelId>>::new();
        let mut defaults_by_provider = HashMap::<ProviderId, Vec<ModelId>>::new();
        let mut small_defaults_by_provider = HashMap::<ProviderId, Vec<ModelId>>::new();

        let mut provider_ids = settings.providers.keys().cloned().collect::<Vec<_>>();
        provider_ids.sort_unstable();
        for provider_id in provider_ids {
            if !known_providers.contains(provider_id.as_str())
                || !enabled_providers.contains(provider_id.as_str())
            {
                continue;
            }
            let provider = provider_by_id
                .get(provider_id.as_str())
                .expect("enabled provider ID should have provider metadata");
            let provider_settings = settings
                .providers
                .get(&provider_id)
                .expect("provider ID came from settings map keys");
            let identifiers = model_identifiers.entry(provider.id.clone()).or_default();
            let mut model_ids = provider_settings.models.keys().cloned().collect::<Vec<_>>();
            model_ids.sort_unstable();
            for model_id in model_ids {
                let model_settings = provider_settings
                    .models
                    .get(&model_id)
                    .expect("model ID came from provider model map keys");
                if model_settings.enabled == Some(false) {
                    continue;
                }

                if let Some((_, canonical_model)) = legacy_builtin_model(&model_id) {
                    return Err(LegacyModelError::LegacyIdentifierAsModelId {
                        identifier: model_id,
                        provider:   provider.id.clone(),
                        model:      canonical_model,
                    }
                    .into());
                }

                let (model, resolved_settings) = build_model(&model_id, model_settings, provider)?;
                register_model_identifier(
                    identifiers,
                    model.id.as_str().to_string(),
                    model.id.clone(),
                    &model.provider,
                )?;
                for alias in &model.aliases {
                    register_model_identifier(
                        identifiers,
                        alias.clone(),
                        model.id.clone(),
                        &model.provider,
                    )?;
                }
                register_model_identifier(
                    identifiers,
                    resolved_settings.api_id.clone(),
                    model.id.clone(),
                    &model.provider,
                )?;

                if model.default {
                    defaults_by_provider
                        .entry(model.provider.clone())
                        .or_default()
                        .push(model.id.clone());
                }
                if model.small_default {
                    small_defaults_by_provider
                        .entry(model.provider.clone())
                        .or_default()
                        .push(model.id.clone());
                }
                models_with_settings.push((model, resolved_settings));
            }
        }

        for (provider, defaults) in defaults_by_provider {
            if defaults.len() > 1 {
                return Err(CatalogBuildError::MultipleProviderDefaults {
                    provider,
                    models: defaults.into_iter().map(ModelId::into_inner).collect(),
                });
            }
        }
        for (provider, small_defaults) in small_defaults_by_provider {
            if small_defaults.len() > 1 {
                return Err(CatalogBuildError::MultipleProviderSmallDefaults {
                    provider,
                    models: small_defaults
                        .into_iter()
                        .map(ModelId::into_inner)
                        .collect(),
                });
            }
        }
        if !models_with_settings.iter().any(|(model, _)| model.default) {
            return Err(CatalogBuildError::NoDefaultModel);
        }

        models_with_settings.sort_by(|(left, _), (right, _)| {
            provider_index[&left.provider]
                .cmp(&provider_index[&right.provider])
                .then_with(|| left.id.cmp(&right.id))
        });
        warn_multiple_probe_models(&models_with_settings);
        let (offering_index, provider_selector_index, canonical_candidates, alias_candidates) =
            build_model_indexes(&models_with_settings);
        let mut model_settings_by_offering = HashMap::new();
        let mut models = Vec::new();
        for (model, settings) in models_with_settings {
            model_settings_by_offering.insert((model.provider.clone(), model.id.clone()), settings);
            models.push(model);
        }

        Ok(Self {
            models,
            providers,
            model_settings: model_settings_by_offering,
            offering_index,
            provider_selector_index,
            canonical_candidates,
            alias_candidates,
            provider_aliases,
            provider_index,
        })
    }

    pub fn from_builtin_with_overrides(
        overrides: &LlmCatalogSettings,
    ) -> Result<Self, CatalogBuildError> {
        let builtins = normalize_catalog_settings(Self::builtin_settings()?, None)?;
        let overrides = normalize_catalog_settings(overrides.clone(), Some(&builtins))?;
        let settings = merge_catalog_settings(overrides, builtins);
        Self::from_settings(&settings)
    }

    /// Builds a fresh catalog from embedded provider TOML without user
    /// overrides.
    pub fn from_builtin() -> Result<Self, CatalogBuildError> {
        Self::from_builtin_toml()
    }

    fn builtin_settings() -> Result<LlmCatalogSettings, CatalogBuildError> {
        let mut layer = LlmCatalogSettings::default();
        let mut paths = BuiltinCatalogToml::iter()
            .filter(|path| path.ends_with(".toml"))
            .map(Cow::into_owned)
            .collect::<Vec<_>>();
        paths.sort_unstable();
        if paths.is_empty() {
            return Err(CatalogBuildError::NoBuiltinProviderFiles);
        }

        for path in paths {
            let file = BuiltinCatalogToml::get(&path)
                .expect("path came from embedded built-in catalog iterator");
            let source = std::str::from_utf8(file.data.as_ref()).map_err(|source| {
                CatalogBuildError::InvalidBuiltinUtf8 {
                    path: path.clone(),
                    source,
                }
            })?;
            let fragment: LlmCatalogSettings =
                toml::from_str(source).map_err(|source| CatalogBuildError::InvalidBuiltinToml {
                    path: path.clone(),
                    source,
                })?;
            validate_builtin_fragment(&path, &fragment)?;
            layer.providers.extend(fragment.providers);
            layer.models.extend(fragment.models);
        }

        normalize_catalog_settings(layer, None)
    }

    fn from_builtin_toml() -> Result<Self, CatalogBuildError> {
        Self::from_settings(&Self::builtin_settings()?)
    }

    /// Test-only shorthand for selecting from every enabled catalog provider.
    ///
    /// Production callers must supply an explicit ready-provider snapshot to
    /// [`Catalog::select`] or use a provider-scoped lookup.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn get(&self, selector: &str) -> Option<&Model> {
        self.candidate_indices(selector)
            .and_then(|indices| indices.first())
            .and_then(|idx| self.models.get(*idx))
    }

    /// Look up a canonical ID, alias, or API ID on exactly one provider,
    /// without considering provider availability. Exact provider-scoped
    /// identifiers win before historical built-in API identifiers normalize
    /// to their canonical model slug.
    #[must_use]
    pub fn get_on_provider(&self, provider: &ProviderId, selector: &str) -> Option<&Model> {
        let provider = self.provider(provider)?;
        let lookup = |selector: &str| {
            self.provider_selector_index
                .get(&(provider.id.clone(), selector.to_string()))
        };
        let index =
            lookup(selector).or_else(|| lookup(&normalize_legacy_builtin_selector(selector)))?;
        self.models.get(*index)
    }

    /// Look up a canonical offering by its composite identity.
    #[must_use]
    pub fn offering(&self, provider: &ProviderId, model: &ModelId) -> Option<&Model> {
        let provider = self.provider(provider)?;
        self.offering_index
            .get(&(provider.id.clone(), model.clone()))
            .and_then(|idx| self.models.get(*idx))
    }

    /// Look up a provider by ID or alias, failing when the catalog has no such
    /// provider.
    pub fn require_provider(
        &self,
        provider: &ProviderId,
    ) -> Result<&CatalogProvider, ModelSelectionError> {
        self.provider(provider)
            .ok_or_else(|| ModelSelectionError::UnknownProvider {
                provider: provider.clone(),
            })
    }

    /// Canonicalize a provider name or alias to its catalog ID.
    pub fn provider_id(&self, name: &str) -> Result<ProviderId, ModelSelectionError> {
        Ok(self.require_provider(&ProviderId::from(name))?.id.clone())
    }

    /// Resolve a canonical ID, alias, or API ID on exactly one provider.
    pub fn resolve_on_provider(
        &self,
        provider: &ProviderId,
        selector: &str,
    ) -> Result<&Model, ModelSelectionError> {
        let provider = self.require_provider(provider)?;
        if let Some(model) = self.get_on_provider(&provider.id, selector) {
            return Ok(model);
        }
        Err(ModelSelectionError::UnknownSelectorOnProvider {
            selector: selector.to_string(),
            provider: provider.id.clone(),
        })
    }

    /// Select one concrete offering for a selector and ready-provider
    /// snapshot.
    ///
    /// Historical built-in API identifiers normalize to their canonical model
    /// slug before selection.
    ///
    /// An explicit provider is a pin and also permits that provider's API IDs.
    /// Unqualified selection checks canonical IDs before aliases and uses the
    /// catalog's provider priority ordering.
    pub fn select<'a>(
        &'a self,
        selector: &str,
        explicit_provider: Option<&ProviderId>,
        eligible_providers: &HashSet<ProviderId>,
    ) -> Result<&'a Model, ModelSelectionError> {
        let eligible = eligible_providers
            .iter()
            .filter_map(|provider| self.provider(provider).map(|provider| provider.id.clone()))
            .collect::<HashSet<_>>();

        if let Some(explicit_provider) = explicit_provider {
            let provider = self.require_provider(explicit_provider)?;
            if !eligible.contains(&provider.id) {
                return Err(ModelSelectionError::ProviderUnavailable {
                    provider: provider.id.clone(),
                });
            }
            return self.resolve_on_provider(&provider.id, selector);
        }

        let normalized_selector = normalize_legacy_builtin_selector(selector);
        let canonical = self
            .canonical_candidates
            .get(&ModelId::new(normalized_selector.as_ref()));
        if let Some(indices) = canonical {
            if let Some(model) = indices
                .iter()
                .filter_map(|idx| self.models.get(*idx))
                .find(|model| eligible.contains(&model.provider))
            {
                return Ok(model);
            }
        }

        let aliases = self.alias_candidates.get(normalized_selector.as_ref());
        if let Some(indices) = aliases {
            if let Some(model) = indices
                .iter()
                .filter_map(|idx| self.models.get(*idx))
                .find(|model| eligible.contains(&model.provider))
            {
                return Ok(model);
            }
        }

        let mut providers = Vec::new();
        for index in canonical
            .into_iter()
            .flatten()
            .chain(aliases.into_iter().flatten())
        {
            let Some(model) = self.models.get(*index) else {
                continue;
            };
            if !providers.contains(&model.provider) {
                providers.push(model.provider.clone());
            }
        }
        if !providers.is_empty() {
            return Err(ModelSelectionError::NoEligibleOffering {
                selector: selector.to_string(),
                providers,
            });
        }

        Err(ModelSelectionError::UnknownSelector {
            selector: selector.to_string(),
        })
    }

    #[must_use]
    pub fn all_provider_ids(&self) -> HashSet<ProviderId> {
        self.providers
            .iter()
            .map(|provider| provider.id.clone())
            .collect()
    }

    /// Canonicalize a model selector to its catalog model ID, preferring the
    /// given provider's offering. Unknown selectors pass through verbatim.
    ///
    /// Model-keyed fallback chains are written and read through this one
    /// function so a configured chain key and a dispatch-time lookup cannot
    /// silently disagree.
    #[must_use]
    pub fn canonical_model_id(&self, provider: &ProviderId, selector: &str) -> String {
        self.get_on_provider(provider, selector).map_or_else(
            || {
                self.select(selector, None, &self.all_provider_ids())
                    .map_or_else(|_| selector.to_string(), |offering| offering.id.to_string())
            },
            |offering| offering.id.to_string(),
        )
    }

    /// Select the highest-priority default model on an eligible provider.
    pub fn select_default(
        &self,
        eligible_providers: &HashSet<ProviderId>,
    ) -> Result<&Model, ModelSelectionError> {
        let eligible = eligible_providers
            .iter()
            .filter_map(|provider| self.provider(provider).map(|provider| provider.id.clone()))
            .collect::<HashSet<_>>();
        if let Some(model) = self
            .models
            .iter()
            .find(|model| model.default && eligible.contains(&model.provider))
        {
            return Ok(model);
        }
        let mut providers = self
            .models
            .iter()
            .filter(|model| model.default)
            .map(|model| model.provider.clone())
            .collect::<Vec<_>>();
        providers.sort();
        providers.dedup();
        Err(ModelSelectionError::NoDefaultModel { providers })
    }

    /// Canonicalize a provider ID or alias and require it to be in the
    /// eligible snapshot.
    pub fn ready_provider(
        &self,
        provider: &ProviderId,
        eligible_providers: &HashSet<ProviderId>,
    ) -> Result<ProviderId, ModelSelectionError> {
        let provider = self.require_provider(provider)?;
        let ready = eligible_providers.iter().any(|eligible| {
            self.provider(eligible)
                .is_some_and(|eligible| eligible.id == provider.id)
        });
        if !ready {
            return Err(ModelSelectionError::ProviderUnavailable {
                provider: provider.id.clone(),
            });
        }
        Ok(provider.id.clone())
    }

    /// Resolve an optional selector to one provider/model pair, applying the
    /// passthrough policy shared by every dispatch boundary:
    ///
    /// - A selector known to the catalog resolves to its canonical offering.
    /// - An unknown selector pinned to a provider passes through verbatim on
    ///   that provider.
    /// - An unqualified unknown selector passes through on the default
    ///   provider.
    /// - No selector picks the default offering (of the pinned provider, when
    ///   one is given).
    pub fn resolve_selection(
        &self,
        selector: Option<&str>,
        explicit_provider: Option<&ProviderId>,
        eligible_providers: &HashSet<ProviderId>,
    ) -> Result<SelectedModel, ModelSelectionError> {
        let Some(selector) = selector else {
            let eligible = match explicit_provider {
                Some(provider) => {
                    HashSet::from([self.ready_provider(provider, eligible_providers)?])
                }
                None => eligible_providers.clone(),
            };
            let offering = self.select_default(&eligible)?;
            return Ok(SelectedModel {
                provider: offering.provider.clone(),
                model:    offering.id.to_string(),
            });
        };
        match self.select(selector, explicit_provider, eligible_providers) {
            Ok(offering) => Ok(SelectedModel {
                provider: offering.provider.clone(),
                model:    offering.id.to_string(),
            }),
            Err(ModelSelectionError::UnknownSelectorOnProvider { provider, .. }) => {
                Ok(SelectedModel {
                    provider,
                    model: selector.to_string(),
                })
            }
            Err(ModelSelectionError::UnknownSelector { .. }) => {
                let default = self.select_default(eligible_providers)?;
                Ok(SelectedModel {
                    provider: default.provider.clone(),
                    model:    selector.to_string(),
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Resolve a selection against a preferred provider snapshot, falling back
    /// to every provider in the catalog only when the preferred set cannot
    /// supply the requested provider or model.
    ///
    /// This is useful for readiness checks: ready providers remain preferred,
    /// while a catalog-only offering can still be selected so the caller can
    /// report why its provider is unavailable. Semantic failures such as an
    /// unknown provider do not fall back.
    pub fn resolve_selection_with_catalog_fallback(
        &self,
        selector: Option<&str>,
        explicit_provider: Option<&ProviderId>,
        preferred_providers: &HashSet<ProviderId>,
    ) -> Result<SelectedModel, ModelSelectionError> {
        match self.resolve_selection(selector, explicit_provider, preferred_providers) {
            Ok(selected) => Ok(selected),
            Err(
                ModelSelectionError::ProviderUnavailable { .. }
                | ModelSelectionError::NoEligibleOffering { .. }
                | ModelSelectionError::NoDefaultModel { .. },
            ) => self.resolve_selection(selector, explicit_provider, &self.all_provider_ids()),
            Err(error) => Err(error),
        }
    }

    #[must_use]
    pub fn is_model_selector(&self, selector: &str) -> bool {
        self.candidate_indices(selector).is_some()
    }

    fn candidate_indices(&self, selector: &str) -> Option<&Vec<usize>> {
        let selector = normalize_legacy_builtin_selector(selector);
        self.canonical_candidates
            .get(&ModelId::new(selector.as_ref()))
            .or_else(|| self.alias_candidates.get(selector.as_ref()))
    }

    #[must_use]
    pub fn providers(&self) -> &[CatalogProvider] {
        &self.providers
    }

    #[must_use]
    pub fn provider_summaries(&self, configured: &HashSet<ProviderId>) -> Vec<Provider> {
        #[derive(Default)]
        struct Stats {
            model_count:   u32,
            default_model: Option<String>,
        }

        let mut stats_by_provider = HashMap::<ProviderId, Stats>::new();
        for model in &self.models {
            let stats = stats_by_provider.entry(model.provider.clone()).or_default();
            stats.model_count = stats.model_count.saturating_add(1);
            if model.default {
                stats.default_model = Some(model.id.to_string());
            }
        }

        self.providers
            .iter()
            .map(|provider| {
                let stats = stats_by_provider.remove(&provider.id).unwrap_or_default();
                Provider::from_catalog(
                    provider,
                    stats.model_count,
                    stats.default_model,
                    configured.contains(&provider.id),
                )
            })
            .collect()
    }

    #[must_use]
    pub fn provider(&self, id: &ProviderId) -> Option<&CatalogProvider> {
        let canonical = self.provider_aliases.get(id.as_str()).unwrap_or(id);
        self.provider_index
            .get(canonical)
            .and_then(|idx| self.providers.get(*idx))
    }

    #[must_use]
    pub fn provider_vault_secret_name(&self, id: &ProviderId) -> Option<&str> {
        self.provider(id)?.vault_secret_name()
    }

    #[must_use]
    pub fn settings_for(&self, model: &Model) -> Option<&CatalogModelSettings> {
        self.model_settings
            .get(&(model.provider.clone(), model.id.clone()))
    }

    /// Test-only shorthand for settings on the highest-priority enabled
    /// offering. Production callers must retain the resolved offering and use
    /// [`Catalog::settings_for`].
    #[cfg(test)]
    #[must_use]
    pub(crate) fn model_settings(
        &self,
        selector: impl AsRef<str>,
    ) -> Option<&CatalogModelSettings> {
        self.get(selector.as_ref())
            .and_then(|model| self.settings_for(model))
    }

    #[must_use]
    pub fn model_settings_on_provider(
        &self,
        provider: &ProviderId,
        selector: &str,
    ) -> Option<&CatalogModelSettings> {
        let model = self.get_on_provider(provider, selector)?;
        self.settings_for(model)
    }

    #[must_use]
    pub fn effective_agent_profile(
        &self,
        provider_id: &ProviderId,
        model_id_or_alias: Option<&str>,
    ) -> Option<AgentProfileKind> {
        let provider = self.provider(provider_id)?;
        let model_profile = model_id_or_alias
            .and_then(|model_id| self.get_on_provider(&provider.id, model_id))
            .and_then(|model| self.settings_for(model))
            .map(|settings| settings.agent_profile);
        Some(model_profile.unwrap_or(provider.agent_profile))
    }

    /// The codec a request for `model_id_or_alias` on `provider_id` speaks:
    /// the model row's codec when one is configured, otherwise the
    /// provider's.
    #[must_use]
    pub fn effective_codec(
        &self,
        provider_id: &ProviderId,
        model_id_or_alias: Option<&str>,
    ) -> Option<CodecKind> {
        let provider = self.provider(provider_id)?;
        let model_codec = model_id_or_alias
            .and_then(|model_id| self.get_on_provider(&provider.id, model_id))
            .and_then(|model| self.settings_for(model))
            .map(|settings| settings.codec);
        Some(model_codec.unwrap_or(provider.codec))
    }

    /// The billing family for `model_id_or_alias` on `provider_id`: the model
    /// row's policy when one is configured, otherwise the provider's (unknown
    /// passthrough model ids keep the provider policy).
    #[must_use]
    pub fn effective_billing_policy(
        &self,
        provider_id: &ProviderId,
        model_id_or_alias: Option<&str>,
    ) -> Option<BillingPolicy> {
        let provider = self.provider(provider_id)?;
        let model_policy = model_id_or_alias
            .and_then(|model_id| self.get_on_provider(&provider.id, model_id))
            .and_then(|model| self.settings_for(model))
            .map(|settings| settings.billing_policy);
        Some(model_policy.unwrap_or(provider.billing_policy))
    }

    /// List all models, optionally filtered by provider.
    #[must_use]
    pub fn list(&self, provider: Option<&ProviderId>) -> Vec<&Model> {
        match provider {
            None => self.models.iter().collect(),
            Some(p) => {
                let provider_id = self.provider(p).map_or(p, |provider| &provider.id);
                self.models
                    .iter()
                    .filter(|m| &m.provider == provider_id)
                    .collect()
            }
        }
    }

    /// The overall default model (first model marked `default` in catalog).
    ///
    /// # Panics
    /// Panics if the catalog contains no default model.
    #[must_use]
    pub fn default_model(&self) -> &Model {
        self.providers
            .iter()
            .find_map(|provider| self.default_for_provider(&provider.id))
            .or_else(|| self.models.iter().find(|m| m.default))
            .expect("catalog must contain at least one default model")
    }

    /// The default model for a specific provider.
    #[must_use]
    pub fn default_for_provider(&self, p: &ProviderId) -> Option<&Model> {
        let provider_id = self
            .provider(p)
            .map_or_else(|| p.clone(), |provider| provider.id.clone());
        self.models
            .iter()
            .find(|m| m.provider == provider_id && m.default)
    }

    /// Small default model for a provider — the small/cheap utility model used
    /// for metadata enrichment. `None` when the provider marks no small
    /// default. Deliberately does not substitute the provider's normal
    /// default, which is typically a large reasoning model: callers asking for
    /// a small model give it a small token budget and a short timeout, and a
    /// flagship model silently exceeds both.
    #[must_use]
    pub fn small_default_for_provider(&self, p: &ProviderId) -> Option<&Model> {
        let provider_id = self.provider(p).map_or(p, |provider| &provider.id);
        self.models
            .iter()
            .find(|m| &m.provider == provider_id && m.small_default)
    }

    /// Default model for the best-available provider (based on API keys),
    /// falling back to the global catalog default.
    #[must_use]
    #[expect(
        clippy::disallowed_methods,
        reason = "Catalog default selection intentionally checks provider API-key env refs."
    )]
    pub fn default_from_env(&self) -> &Model {
        let configured = self
            .providers
            .iter()
            .filter(|provider| {
                provider.auth.as_ref().is_some_and(|auth| {
                    auth.credentials.iter().any(|credential| {
                        matches!(credential, CredentialRef::Env(name) if std::env::var(name).is_ok())
                    })
                })
            })
            .map(|provider| provider.id.clone())
            .collect::<Vec<_>>();
        self.default_for_configured_ids(&configured)
    }

    /// Default model for the best-available built-in provider IDs, falling
    /// back to the global catalog default.
    #[must_use]
    pub fn default_for_configured_ids(&self, configured: &[ProviderId]) -> &Model {
        if configured.is_empty() {
            return self.default_model();
        }
        let configured = self.canonical_provider_ids(configured);
        self.providers
            .iter()
            .filter(|provider| configured.contains(&provider.id))
            .find_map(|provider| self.default_for_provider(&provider.id))
            .unwrap_or_else(|| self.default_model())
    }

    /// Small default model for the best-available built-in provider IDs.
    ///
    /// Configured providers that mark no small default are skipped in favour
    /// of a lower-priority provider that has one. Only when none of them does
    /// is the ordinary default used.
    #[must_use]
    pub fn small_default_for_configured_ids(&self, configured: &[ProviderId]) -> &Model {
        if configured.is_empty() {
            return self.default_model();
        }
        let configured = self.canonical_provider_ids(configured);
        let mut fallback = None;
        for model in self
            .models
            .iter()
            .filter(|model| configured.contains(&model.provider))
        {
            if model.small_default {
                return model;
            }
            if model.default && fallback.is_none() {
                fallback = Some(model);
            }
        }
        fallback.unwrap_or_else(|| self.default_model())
    }

    fn canonical_provider_ids(&self, provider_ids: &[ProviderId]) -> HashSet<ProviderId> {
        provider_ids
            .iter()
            .filter_map(|id| self.provider(id).map(|provider| provider.id.clone()))
            .collect()
    }

    /// Probe model for a provider — the cheapest model suitable for
    /// connectivity checks. Falls back to the provider's default when no
    /// explicit override is configured.
    #[must_use]
    pub fn probe_for_provider(&self, p: &ProviderId) -> Option<&Model> {
        let provider_id = self.provider(p).map_or(p, |provider| &provider.id);
        if let Some(model) = self.models.iter().find(|model| {
            &model.provider == provider_id
                && self
                    .settings_for(model)
                    .is_some_and(|settings| settings.probe)
        }) {
            return Some(model);
        }
        self.default_for_provider(provider_id)
    }

    /// Find the closest model on a target provider matching the reference's
    /// capabilities.
    ///
    /// Hard-filters on `features.tools`, `features.vision`, and
    /// `features.reasoning`. Among matches, picks the closest by
    /// `costs.input_cost_per_mtok` (absolute diff).
    #[must_use]
    pub fn closest(&self, target: &ProviderId, reference: &Model) -> Option<&Model> {
        let target = self
            .provider(target)
            .map_or(target, |provider| &provider.id);
        self.models
            .iter()
            .filter(|m| {
                &m.provider == target
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
}

type ModelIndexes = (
    HashMap<(ProviderId, ModelId), usize>,
    HashMap<(ProviderId, String), usize>,
    HashMap<ModelId, Vec<usize>>,
    HashMap<String, Vec<usize>>,
);

fn build_model_indexes(models: &[(Model, CatalogModelSettings)]) -> ModelIndexes {
    let mut offering_index = HashMap::new();
    let mut provider_selector_index = HashMap::new();
    let mut canonical_candidates = HashMap::<ModelId, Vec<usize>>::new();
    let mut alias_candidates = HashMap::<String, Vec<usize>>::new();
    for (idx, (model, settings)) in models.iter().enumerate() {
        offering_index.insert((model.provider.clone(), model.id.clone()), idx);
        provider_selector_index
            .insert((model.provider.clone(), model.id.as_str().to_string()), idx);
        canonical_candidates
            .entry(model.id.clone())
            .or_default()
            .push(idx);
        for alias in &model.aliases {
            provider_selector_index.insert((model.provider.clone(), alias.clone()), idx);
            alias_candidates.entry(alias.clone()).or_default().push(idx);
        }
        provider_selector_index.insert((model.provider.clone(), settings.api_id.clone()), idx);
    }
    (
        offering_index,
        provider_selector_index,
        canonical_candidates,
        alias_candidates,
    )
}

fn normalize_catalog_settings(
    mut settings: LlmCatalogSettings,
    known: Option<&LlmCatalogSettings>,
) -> Result<LlmCatalogSettings, CatalogBuildError> {
    reject_scoped_provider_fields(&settings)?;

    let legacy_models = std::mem::take(&mut settings.models);
    if legacy_models.is_empty() {
        return Ok(settings);
    }
    let mut legacy_models = legacy_models.into_iter().collect::<Vec<_>>();
    legacy_models.sort_by(|(left, _), (right, _)| left.cmp(right));

    let mut index = LegacyModelIndex::default();
    index.add_settings(&settings);
    if let Some(known) = known {
        index.add_settings(known);
    }

    for (legacy_id, mut model_settings) in legacy_models {
        let explicit_provider = model_settings.provider.take();
        let (provider, model_id) = index.resolve(&legacy_id, explicit_provider.as_deref())?;

        if !settings.providers.contains_key(provider.as_str())
            && !known.is_some_and(|known| known.providers.contains_key(provider.as_str()))
        {
            return Err(CatalogBuildError::UnknownModelProvider {
                model: legacy_id,
                provider,
            });
        }

        let provider_settings = settings.providers.entry(provider.to_string()).or_default();
        if provider_settings.models.contains_key(model_id.as_str()) {
            return Err(LegacyModelError::DuplicateModel {
                provider,
                model: model_id,
            }
            .into());
        }
        provider_settings
            .models
            .insert(model_id.into_inner(), model_settings);
    }
    Ok(settings)
}

fn reject_scoped_provider_fields(settings: &LlmCatalogSettings) -> Result<(), LegacyModelError> {
    for (provider, settings) in &settings.providers {
        for (model, settings) in &settings.models {
            if settings.provider.is_some() {
                return Err(LegacyModelError::ScopedModelDeclaresProvider {
                    provider: ProviderId::new(provider.clone()),
                    model:    ModelId::new(model.clone()),
                });
            }
        }
    }
    Ok(())
}

/// Failure to resolve a legacy top-level `[models.<id>]` row onto its
/// provider.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LegacyModelError {
    #[error("failed to inspect the built-in model catalog: {message}")]
    BuiltinCatalog { message: String },
    #[error(
        "legacy built-in model identifier '{identifier}' cannot be used as a canonical model ID under provider '{provider}'; use '{model}'"
    )]
    LegacyIdentifierAsModelId {
        identifier: String,
        provider:   ProviderId,
        model:      ModelId,
    },
    #[error("legacy model row '{model}' omits provider and does not match a unique known offering")]
    UnknownModel { model: String },
    #[error(
        "legacy model row '{model}' omits provider and matches multiple offerings: {candidates:?}"
    )]
    AmbiguousModel {
        model:      String,
        candidates: Vec<(ProviderId, ModelId)>,
    },
    #[error("legacy model selector '{selector}' is ambiguous on provider '{provider}': {models:?}")]
    AmbiguousAlias {
        provider: ProviderId,
        selector: String,
        models:   Vec<ModelId>,
    },
    #[error("provider-scoped model '{provider}/{model}' must not declare a provider field")]
    ScopedModelDeclaresProvider {
        provider: ProviderId,
        model:    ModelId,
    },
    #[error(
        "provider '{provider}' model '{model}' is defined through both provider-scoped and legacy top-level syntax"
    )]
    DuplicateModel {
        provider: ProviderId,
        model:    ModelId,
    },
}

/// Identifier/alias view used to resolve legacy top-level `[models.<id>]`
/// rows onto their provider before provider-scoped settings merge.
///
/// Both the settings-layer normalization in `fabro-config` and catalog-build
/// normalization here feed this index: local entries first, lower-precedence
/// known entries (e.g. the built-in catalog) after. Canonical IDs always win
/// over aliases; alias ties resolve to the first entry added.
#[derive(Debug, Default)]
pub struct LegacyModelIndex {
    providers: Vec<LegacyProviderEntry>,
}

#[derive(Debug)]
struct LegacyProviderEntry {
    id:      ProviderId,
    aliases: Vec<String>,
    models:  Vec<LegacyModelEntry>,
}

#[derive(Debug)]
struct LegacyModelEntry {
    id:      ModelId,
    aliases: Vec<String>,
}

impl LegacyModelIndex {
    pub fn add_provider(
        &mut self,
        id: ProviderId,
        aliases: Vec<String>,
        models: impl IntoIterator<Item = (ModelId, Vec<String>)>,
    ) {
        self.providers.push(LegacyProviderEntry {
            id,
            aliases,
            models: models
                .into_iter()
                .map(|(id, aliases)| LegacyModelEntry { id, aliases })
                .collect(),
        });
    }

    fn add_settings(&mut self, settings: &LlmCatalogSettings) {
        let mut provider_ids = settings.providers.keys().collect::<Vec<_>>();
        provider_ids.sort_unstable();
        for provider_id in provider_ids {
            let provider = &settings.providers[provider_id];
            let mut model_ids = provider.models.keys().collect::<Vec<_>>();
            model_ids.sort_unstable();
            self.add_provider(
                ProviderId::new(provider_id.clone()),
                provider.aliases.clone().unwrap_or_default(),
                model_ids.into_iter().map(|model_id| {
                    let model = &provider.models[model_id];
                    (
                        ModelId::new(model_id.clone()),
                        model.aliases.clone().unwrap_or_default(),
                    )
                }),
            );
        }
    }

    /// Append the built-in catalog as the lowest-precedence tier. Includes
    /// disabled providers because config compatibility normalization happens
    /// before runtime availability is known.
    pub fn with_builtin(mut self) -> Result<Self, LegacyModelError> {
        let builtin =
            Catalog::builtin_settings().map_err(|error| LegacyModelError::BuiltinCatalog {
                message: error.to_string(),
            })?;
        self.add_settings(&builtin);
        Ok(self)
    }

    /// Resolve one legacy row to its provider-scoped address. Historical
    /// built-in identifiers normalize to their canonical slug and use their
    /// historical provider when no explicit provider is present. Other
    /// unknown explicit providers or model selectors pass through verbatim;
    /// rows without an explicit provider must match exactly one known
    /// offering.
    pub fn resolve(
        &self,
        legacy_id: &str,
        explicit_provider: Option<&str>,
    ) -> Result<(ProviderId, ModelId), LegacyModelError> {
        if let Some((historical_provider, model)) = legacy_builtin_model(legacy_id) {
            let provider = explicit_provider.map_or(historical_provider, |explicit| {
                self.canonical_provider(explicit)
                    .unwrap_or_else(|| ProviderId::new(explicit))
            });
            return Ok((provider, model));
        }
        if let Some(explicit) = explicit_provider {
            let provider = self
                .canonical_provider(explicit)
                .unwrap_or_else(|| ProviderId::new(explicit));
            let model = self
                .canonical_model_on(&provider, legacy_id)?
                .unwrap_or_else(|| ModelId::new(legacy_id));
            return Ok((provider, model));
        }
        let candidates = self.candidates(legacy_id);
        match candidates.as_slice() {
            [(provider, model)] => Ok((provider.clone(), model.clone())),
            [] => Err(LegacyModelError::UnknownModel {
                model: legacy_id.to_string(),
            }),
            _ => Err(LegacyModelError::AmbiguousModel {
                model: legacy_id.to_string(),
                candidates,
            }),
        }
    }

    fn canonical_provider(&self, selector: &str) -> Option<ProviderId> {
        self.providers
            .iter()
            .find(|provider| provider.id.as_str() == selector)
            .or_else(|| {
                self.providers
                    .iter()
                    .find(|provider| provider.aliases.iter().any(|alias| alias == selector))
            })
            .map(|provider| provider.id.clone())
    }

    fn canonical_model_on(
        &self,
        provider: &ProviderId,
        selector: &str,
    ) -> Result<Option<ModelId>, LegacyModelError> {
        let models = || {
            self.providers
                .iter()
                .filter(|entry| entry.id == *provider)
                .flat_map(|entry| entry.models.iter())
        };
        if models().any(|model| model.id.as_str() == selector) {
            return Ok(Some(ModelId::new(selector)));
        }
        let matches = models()
            .filter(|model| model.aliases.iter().any(|alias| alias == selector))
            .map(|model| model.id.clone())
            .collect::<BTreeSet<_>>();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(matches.into_iter().next()),
            _ => Err(LegacyModelError::AmbiguousAlias {
                provider: provider.clone(),
                selector: selector.to_string(),
                models:   matches.into_iter().collect(),
            }),
        }
    }

    fn candidates(&self, selector: &str) -> Vec<(ProviderId, ModelId)> {
        let canonical = self
            .providers
            .iter()
            .filter(|entry| {
                entry
                    .models
                    .iter()
                    .any(|model| model.id.as_str() == selector)
            })
            .map(|entry| (entry.id.clone(), ModelId::new(selector)))
            .collect::<BTreeSet<_>>();
        if !canonical.is_empty() {
            return canonical.into_iter().collect();
        }
        self.providers
            .iter()
            .flat_map(|entry| {
                entry
                    .models
                    .iter()
                    .filter(|model| model.aliases.iter().any(|alias| alias == selector))
                    .map(|model| (entry.id.clone(), model.id.clone()))
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// Historical built-in catalog keys from before Fabro separated canonical
/// model slugs from provider API identifiers. The provider records the key's
/// original offering for legacy catalog-row normalization; runtime selectors
/// normalize to the model slug and use normal provider-aware selection.
const LEGACY_BUILTIN_MODEL_IDENTIFIERS: &[(&str, &str, &str)] = &[
    ("openai.gpt-5.5", "bedrock-openai", "gpt-5.5"),
    ("openai.gpt-5.4", "bedrock-openai", "gpt-5.4"),
    (
        "us.anthropic.claude-sonnet-4-6",
        "bedrock",
        "claude-sonnet-4-6",
    ),
    ("us.anthropic.claude-opus-4-8", "bedrock", "claude-opus-4-8"),
    (
        "us.anthropic.claude-haiku-4-5",
        "bedrock",
        "claude-haiku-4-5",
    ),
    ("openai.gpt-oss-120b", "bedrock", "gpt-oss-120b"),
    ("openai.gpt-oss-20b", "bedrock", "gpt-oss-20b"),
    ("amazon.nova-2-lite", "bedrock", "nova-2-lite"),
    ("meta.llama4-maverick", "bedrock", "llama-4-maverick"),
    ("mistral.mistral-large-3", "bedrock", "mistral-large-3"),
    ("mistral.devstral-2", "bedrock", "devstral-2"),
    ("deepseek.v3-2", "bedrock", "deepseek-v3.2"),
    ("moonshotai.kimi-k2.5", "bedrock", "kimi-k2.5"),
    ("zai.glm-5", "bedrock", "glm-5"),
    ("minimax.minimax-m2.5", "bedrock", "minimax-m2.5"),
    ("nvidia.nemotron-3-super", "bedrock", "nemotron-3-super"),
    ("us.anthropic.claude-fable-5", "bedrock", "claude-fable-5"),
    ("anthropic/claude-fable-5", "openrouter", "claude-fable-5"),
    ("anthropic/claude-opus-4-8", "openrouter", "claude-opus-4-8"),
    ("anthropic/claude-opus-4-7", "openrouter", "claude-opus-4-7"),
    (
        "anthropic/claude-sonnet-4-6",
        "openrouter",
        "claude-sonnet-4-6",
    ),
    (
        "anthropic/claude-haiku-4-5",
        "openrouter",
        "claude-haiku-4-5",
    ),
    ("openai/gpt-5.6-sol", "openrouter", "gpt-5.6-sol"),
    ("openai/gpt-5.6-terra", "openrouter", "gpt-5.6-terra"),
    ("openai/gpt-5.6-luna", "openrouter", "gpt-5.6-luna"),
    ("openai/gpt-5.4", "openrouter", "gpt-5.4"),
    ("openai/gpt-5.5", "openrouter", "gpt-5.5"),
    (
        "google/gemini-3.1-pro-preview",
        "openrouter",
        "gemini-3.1-pro-preview",
    ),
    ("google/gemini-3.5-flash", "openrouter", "gemini-3.5-flash"),
    ("xiaomi/mimo-v2.5-pro", "openrouter", "mimo-v2.5-pro"),
    ("minimax/minimax-m2.7", "openrouter", "minimax-m2.7"),
    ("deepseek/deepseek-v4-pro", "openrouter", "deepseek-v4-pro"),
    (
        "deepseek/deepseek-v4-flash",
        "openrouter",
        "deepseek-v4-flash",
    ),
    ("moonshotai/kimi-k2.6", "openrouter", "kimi-k2.6"),
    ("moonshotai/kimi-k3", "openrouter", "kimi-k3"),
    ("poolside/laguna-s-2.1", "openrouter", "laguna-s-2.1"),
    ("poolside/laguna-xs-2.1", "openrouter", "laguna-xs-2.1"),
    ("qwen/qwen3-coder", "openrouter", "qwen3-coder"),
    ("qwen/qwen3.6-flash", "openrouter", "qwen3.6-flash"),
    ("z-ai/glm-5.3", "openrouter", "glm-5.3"),
    ("z-ai/glm-5.3-flash", "openrouter", "glm-5.3-flash"),
    ("z-ai/glm-5.2", "openrouter", "glm-5.2"),
    ("z-ai/glm-4.6", "openrouter", "glm-4.6"),
    (
        "nvidia/nemotron-3-super-120b-a12b",
        "openrouter",
        "nemotron-3-super-120b-a12b",
    ),
    ("mistralai/devstral-2512", "openrouter", "devstral-2512"),
];

/// Return the historical provider and canonical model slug for a legacy
/// built-in catalog key.
#[must_use]
pub fn legacy_builtin_model(identifier: &str) -> Option<(ProviderId, ModelId)> {
    LEGACY_BUILTIN_MODEL_IDENTIFIERS
        .iter()
        .find(|(legacy, _, _)| *legacy == identifier)
        .map(|(_, provider, model)| (ProviderId::new(*provider), ModelId::new(*model)))
}

fn normalize_legacy_builtin_selector(selector: &str) -> Cow<'_, str> {
    legacy_builtin_model(selector).map_or_else(
        || Cow::Borrowed(selector),
        |(_, model)| Cow::Owned(model.into_inner()),
    )
}

fn merge_catalog_settings(
    higher: LlmCatalogSettings,
    mut fallback: LlmCatalogSettings,
) -> LlmCatalogSettings {
    for (id, provider) in higher.providers {
        let provider = match fallback.providers.remove(&id) {
            Some(fallback_provider) => merge_provider_settings(provider, fallback_provider),
            None => provider,
        };
        fallback.providers.insert(id, provider);
    }

    fallback
}

fn merge_provider_settings(
    mut higher: ProviderCatalogSettings,
    mut fallback: ProviderCatalogSettings,
) -> ProviderCatalogSettings {
    for (id, model) in higher.models.drain() {
        let model = match fallback.models.remove(&id) {
            Some(fallback_model) => merge_model_settings(model, fallback_model),
            None => model,
        };
        fallback.models.insert(id, model);
    }
    ProviderCatalogSettings {
        display_name:   higher.display_name.or(fallback.display_name),
        adapter:        higher.adapter.or(fallback.adapter),
        codec:          higher.codec.or(fallback.codec),
        agent_profile:  higher.agent_profile.or(fallback.agent_profile),
        auth:           higher.auth.or(fallback.auth),
        billing_policy: higher.billing_policy.or(fallback.billing_policy),
        api_key_url:    higher.api_key_url.or(fallback.api_key_url),
        base_url:       higher.base_url.or(fallback.base_url),
        extra_headers:  higher.extra_headers.or(fallback.extra_headers),
        priority:       higher.priority.or(fallback.priority),
        enabled:        higher.enabled.or(fallback.enabled),
        aliases:        higher.aliases.or(fallback.aliases),
        models:         fallback.models,
    }
}

fn merge_model_settings(
    higher: ModelCatalogSettings,
    fallback: ModelCatalogSettings,
) -> ModelCatalogSettings {
    ModelCatalogSettings {
        provider:             higher.provider.or(fallback.provider),
        api_id:               higher.api_id.or(fallback.api_id),
        codec:                higher.codec.or(fallback.codec),
        billing_policy:       higher.billing_policy.or(fallback.billing_policy),
        agent_profile:        higher.agent_profile.or(fallback.agent_profile),
        display_name:         higher.display_name.or(fallback.display_name),
        family:               higher.family.or(fallback.family),
        training:             higher.training.or(fallback.training),
        knowledge_cutoff:     higher.knowledge_cutoff.or(fallback.knowledge_cutoff),
        default:              higher.default.or(fallback.default),
        small_default:        higher.small_default.or(fallback.small_default),
        probe:                higher.probe.or(fallback.probe),
        enabled:              higher.enabled.or(fallback.enabled),
        aliases:              higher.aliases.or(fallback.aliases),
        estimated_output_tps: higher
            .estimated_output_tps
            .or(fallback.estimated_output_tps),
        limits:               merge_optional(
            higher.limits,
            fallback.limits,
            merge_model_limits_settings,
        ),
        features:             merge_optional(
            higher.features,
            fallback.features,
            merge_model_features_settings,
        ),
        controls:             merge_optional(
            higher.controls,
            fallback.controls,
            merge_model_controls_settings,
        ),
        costs:                merge_optional(higher.costs, fallback.costs, merge_model_cost_table),
    }
}

fn merge_optional<T>(higher: Option<T>, fallback: Option<T>, merge: fn(&T, &T) -> T) -> Option<T> {
    match (higher, fallback) {
        (Some(higher), Some(fallback)) => Some(merge(&higher, &fallback)),
        (Some(higher), None) => Some(higher),
        (None, fallback) => fallback,
    }
}

fn merge_model_limits_settings(
    higher: &SettingsModelLimits,
    fallback: &SettingsModelLimits,
) -> SettingsModelLimits {
    SettingsModelLimits {
        context_window: higher.context_window.or(fallback.context_window),
        max_output:     higher.max_output.or(fallback.max_output),
    }
}

fn merge_model_features_settings(
    higher: &SettingsModelFeatures,
    fallback: &SettingsModelFeatures,
) -> SettingsModelFeatures {
    SettingsModelFeatures {
        tools:                     higher.tools.or(fallback.tools),
        vision:                    higher.vision.or(fallback.vision),
        reasoning:                 higher.reasoning.or(fallback.reasoning),
        reasoning_by_default:      higher
            .reasoning_by_default
            .or(fallback.reasoning_by_default),
        reasoning_effort:          higher.reasoning_effort.or(fallback.reasoning_effort),
        prompt_cache:              higher.prompt_cache.or(fallback.prompt_cache),
        cache_control_breakpoints: higher
            .cache_control_breakpoints
            .or(fallback.cache_control_breakpoints),
        sampling_params:           higher.sampling_params.or(fallback.sampling_params),
    }
}

fn merge_model_controls_settings(
    higher: &SettingsModelControls,
    fallback: &SettingsModelControls,
) -> SettingsModelControls {
    SettingsModelControls {
        reasoning_effort: higher
            .reasoning_effort
            .clone()
            .or_else(|| fallback.reasoning_effort.clone()),
        speed:            higher.speed.clone().or_else(|| fallback.speed.clone()),
    }
}

fn merge_model_cost_table(
    higher: &SettingsModelCostTable,
    fallback: &SettingsModelCostTable,
) -> SettingsModelCostTable {
    SettingsModelCostTable {
        base:  merge_cost_rates(&higher.base, &fallback.base),
        speed: higher.speed.clone().or_else(|| fallback.speed.clone()),
    }
}

fn merge_cost_rates(higher: &CostRates, fallback: &CostRates) -> CostRates {
    CostRates {
        input_cost_per_mtok:       higher.input_cost_per_mtok.or(fallback.input_cost_per_mtok),
        output_cost_per_mtok:      higher
            .output_cost_per_mtok
            .or(fallback.output_cost_per_mtok),
        cache_input_cost_per_mtok: higher
            .cache_input_cost_per_mtok
            .or(fallback.cache_input_cost_per_mtok),
    }
}

fn build_providers(
    settings: &LlmCatalogSettings,
) -> Result<Vec<CatalogProvider>, CatalogBuildError> {
    let mut providers = Vec::new();
    let mut ids = settings.providers.keys().cloned().collect::<Vec<_>>();
    ids.sort_unstable();
    for id in ids {
        let provider_id = ProviderId::from(id.clone());
        let settings = settings
            .providers
            .get(&id)
            .expect("provider ID came from settings map keys");
        if settings.enabled == Some(false) {
            continue;
        }

        let adapter_name =
            required_provider_string(&provider_id, settings.adapter.as_ref(), "adapter")?;
        let adapter = AdapterKind::from_str(&adapter_name).map_err(|_| {
            CatalogBuildError::UnknownAdapter {
                provider: provider_id.clone(),
                adapter:  adapter_name,
            }
        })?;
        let defaults = adapter_defaults(adapter);
        let codec = resolve_provider_codec(&provider_id, adapter, settings.codec)?;
        let agent_profile = settings.agent_profile.unwrap_or(defaults.agent_profile);
        let auth = settings.auth.clone();
        validate_provider_auth(&provider_id, adapter, auth.as_ref())?;

        providers.push(CatalogProvider {
            id: provider_id,
            display_name: settings.display_name.clone().unwrap_or_else(|| id.clone()),
            adapter,
            codec,
            agent_profile,
            auth,
            billing_policy: settings.billing_policy.unwrap_or(defaults.billing_policy),
            api_key_url: settings.api_key_url.clone(),
            base_url: settings.base_url.clone(),
            extra_headers: settings.extra_headers.clone().unwrap_or_default(),
            priority: settings.priority.unwrap_or_default(),
            aliases: settings.aliases.clone().unwrap_or_default(),
        });
    }
    Ok(providers)
}

#[derive(Debug, Clone, Copy)]
struct AdapterDefaults {
    agent_profile:  AgentProfileKind,
    billing_policy: BillingPolicy,
}

fn adapter_defaults(adapter: AdapterKind) -> AdapterDefaults {
    match adapter {
        // Bedrock hosts Anthropic-family models, so it shares the Anthropic
        // agent profile and billing policy by default.
        AdapterKind::Anthropic | AdapterKind::Bedrock => AdapterDefaults {
            agent_profile:  AgentProfileKind::Anthropic,
            billing_policy: BillingPolicy::Anthropic,
        },
        AdapterKind::OpenAi | AdapterKind::OpenAiCompatible => AdapterDefaults {
            agent_profile:  AgentProfileKind::OpenAi,
            billing_policy: BillingPolicy::OpenAi,
        },
        AdapterKind::Gemini => AdapterDefaults {
            agent_profile:  AgentProfileKind::Gemini,
            billing_policy: BillingPolicy::Gemini,
        },
    }
}

/// Resolve a provider row's codec, rejecting pairings outside the adapter's
/// default so no new route combination is silently enabled by configuration.
fn resolve_provider_codec(
    provider: &ProviderId,
    adapter: AdapterKind,
    configured: Option<CodecKind>,
) -> Result<CodecKind, CatalogBuildError> {
    let expected = CodecKind::default_for(adapter);
    match configured {
        Some(codec) if codec != expected => Err(CatalogBuildError::UnsupportedProviderCodec {
            provider: provider.clone(),
            adapter,
            codec,
            expected,
        }),
        _ => Ok(expected),
    }
}

/// Resolve a model row's codec against its provider, with the same
/// only-the-default-pairing rule as [`resolve_provider_codec`].
fn resolve_model_codec(
    model_id: &str,
    provider: &CatalogProvider,
    configured: Option<CodecKind>,
) -> Result<CodecKind, CatalogBuildError> {
    let expected = CodecKind::default_for(provider.adapter);
    match configured {
        Some(codec) if codec != expected => Err(CatalogBuildError::UnsupportedModelCodec {
            model: model_id.to_string(),
            adapter: provider.adapter,
            codec,
            expected,
        }),
        Some(codec) => Ok(codec),
        None => Ok(provider.codec),
    }
}

fn validate_provider_auth(
    provider: &ProviderId,
    adapter: AdapterKind,
    auth: Option<&ProviderAuthConfig>,
) -> Result<(), CatalogBuildError> {
    match auth {
        Some(auth) if auth.credentials.is_empty() => {
            Err(CatalogBuildError::EmptyApiKeyCredentials {
                provider: provider.clone(),
            })
        }
        Some(auth)
            if adapter != AdapterKind::Bedrock
                && auth
                    .credentials
                    .iter()
                    .any(|credential| matches!(credential, CredentialRef::AwsSigv4)) =>
        {
            Err(CatalogBuildError::UnsupportedAwsSigv4Credential {
                provider: provider.clone(),
                adapter,
            })
        }
        _ => Ok(()),
    }
}

fn build_provider_aliases(
    providers: &[CatalogProvider],
) -> Result<HashMap<String, ProviderId>, CatalogBuildError> {
    let mut identifiers = BTreeMap::<String, ProviderId>::new();
    for provider in providers {
        register_provider_identifier(
            &mut identifiers,
            provider.id.as_str().to_string(),
            provider.id.clone(),
        )?;
        for alias in &provider.aliases {
            register_provider_identifier(&mut identifiers, alias.clone(), provider.id.clone())?;
        }
    }
    Ok(identifiers.into_iter().collect())
}

fn build_model(
    model_id: &str,
    settings: &ModelCatalogSettings,
    provider: &CatalogProvider,
) -> Result<(Model, CatalogModelSettings), CatalogBuildError> {
    let family = required_model_string(model_id, settings.family.as_ref(), "family")?;
    let display_name =
        required_model_string(model_id, settings.display_name.as_ref(), "display_name")?;
    let limits = settings
        .limits
        .as_ref()
        .ok_or_else(|| CatalogBuildError::MissingModelField {
            model: model_id.to_string(),
            field: "limits",
        })?;
    let context_window =
        limits
            .context_window
            .ok_or_else(|| CatalogBuildError::MissingModelField {
                model: model_id.to_string(),
                field: "limits.context_window",
            })?;
    let features =
        settings
            .features
            .as_ref()
            .ok_or_else(|| CatalogBuildError::MissingModelField {
                model: model_id.to_string(),
                field: "features",
            })?;
    let model_features = build_model_features(model_id, features)?;
    let reasoning_by_default = features
        .reasoning_by_default
        .unwrap_or_else(|| model_features.supports_reasoning_effort());
    if reasoning_by_default && !model_features.reasoning {
        return Err(CatalogBuildError::DefaultReasoningWithoutReasoning {
            model: model_id.to_string(),
        });
    }
    let controls = build_model_controls(model_id, &model_features, settings)?;
    let costs = build_model_costs(settings.costs.as_ref());
    let speed_costs = build_speed_costs(model_id, settings.costs.as_ref(), &controls)?;

    let model = Model {
        id: ModelId::new(model_id),
        provider: provider.id.clone(),
        family,
        display_name,
        limits: ModelLimits {
            context_window,
            max_output: limits.max_output,
        },
        training: settings.training.clone(),
        knowledge_cutoff: settings.knowledge_cutoff.clone(),
        features: model_features,
        controls: ModelControls {
            reasoning_effort: controls.reasoning_effort.clone(),
        },
        costs,
        estimated_output_tps: settings.estimated_output_tps,
        aliases: settings.aliases.clone().unwrap_or_default(),
        default: settings.default.unwrap_or_default(),
        small_default: settings.small_default.unwrap_or_default(),
        configured: false,
    };
    let api_id = match settings.api_id.as_ref() {
        Some(api_id) if api_id.is_empty() => {
            return Err(CatalogBuildError::EmptyModelApiId {
                provider: provider.id.clone(),
                model:    ModelId::new(model_id),
            });
        }
        Some(api_id) => api_id.clone(),
        None => model_id.to_string(),
    };
    let catalog_settings = CatalogModelSettings {
        api_id,
        codec: resolve_model_codec(model_id, provider, settings.codec)?,
        billing_policy: settings.billing_policy.unwrap_or(provider.billing_policy),
        agent_profile: settings.agent_profile.unwrap_or(provider.agent_profile),
        reasoning_by_default,
        controls,
        speed_costs,
        probe: settings.probe.unwrap_or_default(),
    };
    Ok((model, catalog_settings))
}

fn warn_multiple_probe_models(models_with_settings: &[(Model, CatalogModelSettings)]) {
    let mut probes_by_provider = BTreeMap::<ProviderId, Vec<String>>::new();
    for (model, settings) in models_with_settings {
        if settings.probe {
            probes_by_provider
                .entry(model.provider.clone())
                .or_default()
                .push(model.id.to_string());
        }
    }

    for (provider, models) in probes_by_provider {
        if models.len() > 1 {
            warn!(
                provider = %provider,
                models = ?models,
                "Multiple probe models configured for provider"
            );
        }
    }
}

fn build_model_features(
    model_id: &str,
    features: &SettingsModelFeatures,
) -> Result<ModelFeatures, CatalogBuildError> {
    let reasoning = features
        .reasoning
        .ok_or_else(|| CatalogBuildError::MissingModelField {
            model: model_id.to_string(),
            field: "features.reasoning",
        })?;
    let reasoning_effort = features.reasoning_effort.unwrap_or_default();
    if !reasoning && reasoning_effort != ReasoningEffortFeature::None {
        return Err(CatalogBuildError::ReasoningEffortWithoutReasoning {
            model: model_id.to_string(),
        });
    }
    let prompt_cache = features.prompt_cache.unwrap_or_default();
    let cache_control_breakpoints = features.cache_control_breakpoints.unwrap_or_default();
    if cache_control_breakpoints && !prompt_cache {
        return Err(
            CatalogBuildError::CacheControlBreakpointsWithoutPromptCache {
                model: model_id.to_string(),
            },
        );
    }

    Ok(ModelFeatures {
        tools: features
            .tools
            .ok_or_else(|| CatalogBuildError::MissingModelField {
                model: model_id.to_string(),
                field: "features.tools",
            })?,
        vision: features
            .vision
            .ok_or_else(|| CatalogBuildError::MissingModelField {
                model: model_id.to_string(),
                field: "features.vision",
            })?,
        reasoning,
        reasoning_effort,
        prompt_cache,
        cache_control_breakpoints,
        sampling_params: features.sampling_params.unwrap_or(true),
    })
}

fn build_model_costs(costs: Option<&SettingsModelCostTable>) -> ModelCosts {
    let base = costs.map(|costs| &costs.base);
    ModelCosts {
        input_cost_per_mtok:       base.and_then(|base| base.input_cost_per_mtok),
        output_cost_per_mtok:      base.and_then(|base| base.output_cost_per_mtok),
        cache_input_cost_per_mtok: base.and_then(|base| base.cache_input_cost_per_mtok),
    }
}

fn build_speed_costs(
    model_id: &str,
    costs: Option<&SettingsModelCostTable>,
    controls: &CatalogModelControls,
) -> Result<HashMap<Speed, ModelCosts>, CatalogBuildError> {
    let mut speed_costs = HashMap::new();
    let Some(costs) = costs.and_then(|costs| costs.speed.as_ref()) else {
        return Ok(speed_costs);
    };
    for (speed, rates) in costs {
        let speed = parse_speed(model_id, speed)?;
        if !controls.speed.contains(&speed) {
            return Err(CatalogBuildError::UndeclaredSpeedCost {
                model: model_id.to_string(),
                speed,
            });
        }
        speed_costs.insert(speed, cost_rates_to_model_costs(rates));
    }
    Ok(speed_costs)
}

fn cost_rates_to_model_costs(rates: &CostRates) -> ModelCosts {
    ModelCosts {
        input_cost_per_mtok:       rates.input_cost_per_mtok,
        output_cost_per_mtok:      rates.output_cost_per_mtok,
        cache_input_cost_per_mtok: rates.cache_input_cost_per_mtok,
    }
}

fn build_model_controls(
    model_id: &str,
    features: &ModelFeatures,
    settings: &ModelCatalogSettings,
) -> Result<CatalogModelControls, CatalogBuildError> {
    let supports_native_reasoning_effort = features.supports_reasoning_effort();
    let reasoning_effort = match settings
        .controls
        .as_ref()
        .and_then(|controls| controls.reasoning_effort.as_ref())
    {
        Some(values) if !features.reasoning && !values.is_empty() => {
            return Err(CatalogBuildError::ReasoningEffortControlsWithoutReasoning {
                model: model_id.to_string(),
            });
        }
        Some(values) if values.is_empty() && supports_native_reasoning_effort => {
            return Err(CatalogBuildError::EmptyReasoningEffortControls {
                model: model_id.to_string(),
            });
        }
        Some(values) => values
            .iter()
            .map(|value| parse_reasoning_effort(model_id, value))
            .collect::<Result<Vec<_>, _>>()?,
        None if supports_native_reasoning_effort => ReasoningEffort::VARIANTS.to_vec(),
        None => Vec::new(),
    };

    let speed = settings
        .controls
        .as_ref()
        .and_then(|controls| controls.speed.as_ref())
        .map(|values| {
            values
                .iter()
                .map(|value| parse_speed_control(model_id, value))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();

    Ok(CatalogModelControls {
        reasoning_effort,
        speed,
    })
}

fn parse_reasoning_effort(
    model_id: &str,
    value: &str,
) -> Result<ReasoningEffort, CatalogBuildError> {
    ReasoningEffort::from_str(value).map_err(|source| CatalogBuildError::InvalidReasoningEffort {
        model: model_id.to_string(),
        value: value.to_string(),
        source,
    })
}

fn parse_speed(model_id: &str, value: &str) -> Result<Speed, CatalogBuildError> {
    Speed::from_str(value).map_err(|source| CatalogBuildError::InvalidSpeed {
        model: model_id.to_string(),
        value: value.to_string(),
        source,
    })
}

fn parse_speed_control(model_id: &str, value: &str) -> Result<Speed, CatalogBuildError> {
    let speed = parse_speed(model_id, value)?;
    if speed == Speed::Standard {
        return Err(CatalogBuildError::StandardSpeedControl {
            model: model_id.to_string(),
        });
    }
    Ok(speed)
}

fn required_provider_string(
    provider: &ProviderId,
    value: Option<&String>,
    field: &'static str,
) -> Result<String, CatalogBuildError> {
    value
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| CatalogBuildError::MissingProviderField {
            provider: provider.clone(),
            field,
        })
}

fn required_model_string(
    model: &str,
    value: Option<&String>,
    field: &'static str,
) -> Result<String, CatalogBuildError> {
    value
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| CatalogBuildError::MissingModelField {
            model: model.to_string(),
            field,
        })
}

fn register_provider_identifier(
    identifiers: &mut BTreeMap<String, ProviderId>,
    identifier: String,
    owner: ProviderId,
) -> Result<(), CatalogBuildError> {
    match identifiers.get(&identifier) {
        Some(existing) if existing != &owner => {
            Err(CatalogBuildError::DuplicateProviderIdentifier {
                identifier,
                first: existing.clone(),
                second: owner,
            })
        }
        _ => {
            identifiers.insert(identifier, owner);
            Ok(())
        }
    }
}

fn register_model_identifier(
    identifiers: &mut BTreeMap<String, ModelId>,
    identifier: String,
    owner: ModelId,
    provider: &ProviderId,
) -> Result<(), CatalogBuildError> {
    match identifiers.get(&identifier) {
        Some(existing) if existing != &owner => {
            Err(CatalogBuildError::DuplicateProviderModelSelector {
                provider: provider.clone(),
                selector: identifier,
                first:    existing.clone(),
                second:   owner,
            })
        }
        _ => {
            identifiers.insert(identifier, owner);
            Ok(())
        }
    }
}

fn validate_builtin_fragment(
    path: &str,
    fragment: &LlmCatalogSettings,
) -> Result<(), CatalogBuildError> {
    if fragment.providers.len() != 1 {
        return Err(CatalogBuildError::InvalidBuiltinProviderCount {
            path: path.to_string(),
        });
    }
    let expected = path
        .strip_suffix(".toml")
        .unwrap_or(path)
        .rsplit('/')
        .next()
        .unwrap_or(path);
    let actual = fragment
        .providers
        .keys()
        .next()
        .expect("provider count was checked");
    if actual != expected {
        return Err(CatalogBuildError::BuiltinProviderIdMismatch {
            path:     path.to_string(),
            expected: expected.to_string(),
            actual:   actual.clone(),
        });
    }

    for (model, settings) in &fragment.models {
        let Some(provider) = settings.provider.as_ref() else {
            continue;
        };
        if provider != expected {
            return Err(CatalogBuildError::BuiltinModelProviderMismatch {
                path:     path.to_string(),
                model:    model.clone(),
                expected: expected.to_string(),
                actual:   provider.clone(),
            });
        }
    }
    Ok(())
}

fn provider_order(left: &CatalogProvider, right: &CatalogProvider) -> std::cmp::Ordering {
    right
        .priority
        .cmp(&left.priority)
        .then_with(|| left.id.cmp(&right.id))
}

#[cfg(test)]
mod tests {
    use strum::VariantArray;

    use super::*;
    use crate::adapter::AdapterKind;
    use crate::reasoning::ReasoningEffort;
    use crate::{AgentProfileKind, ProviderId, Speed};

    fn minimal_settings(source: &str) -> LlmCatalogSettings {
        toml::from_str(source).expect("fixture should parse as an LLM settings layer")
    }

    fn portable_model_catalog() -> Catalog {
        Catalog::from_settings(&minimal_settings(
            r#"
[providers.openai]
display_name = "OpenAI"
adapter = "openai"
agent_profile = "openai"
priority = 90

[providers.openai.models."gpt-5.6-sol"]
display_name = "GPT-5.6 Sol"
family = "gpt-5"
aliases = ["gpt-56-sol", "portable"]
default = true

[providers.openai.models."gpt-5.6-sol".limits]
context_window = 1000

[providers.openai.models."gpt-5.6-sol".features]
tools = true
vision = false
reasoning = true

[providers.openrouter]
display_name = "OpenRouter"
adapter = "openai_compatible"
agent_profile = "openai"
priority = 25

[providers.openrouter.models."gpt-5.6-sol"]
api_id = "openai/gpt-5.6-sol"
display_name = "GPT-5.6 Sol (via OpenRouter)"
family = "gpt-5"
aliases = ["gpt-56-sol", "portable"]
default = true

[providers.openrouter.models."gpt-5.6-sol".limits]
context_window = 1000

[providers.openrouter.models."gpt-5.6-sol".features]
tools = true
vision = false
reasoning = true
"#,
        ))
        .expect("portable model fixture should build")
    }

    const BEDROCK_SIGV4_LAYER: &str = r#"
[providers.bedrock]
adapter = "bedrock"
base_url = "https://bedrock-runtime.eu-west-1.amazonaws.com"

[providers.bedrock.auth]
credentials = ["aws_sigv4"]

[models."bedrock-sonnet"]
provider = "bedrock"
api_id = "anthropic.claude-sonnet-4-6"
display_name = "Bedrock Sonnet"
family = "claude-4"
default = true

[models."bedrock-sonnet".limits]
context_window = 200000
max_output = 64000

[models."bedrock-sonnet".features]
tools = true
vision = true
reasoning = true
"#;

    #[test]
    fn provider_parses_bedrock_base_url_and_sigv4_credential() {
        let catalog = Catalog::from_settings(&minimal_settings(BEDROCK_SIGV4_LAYER)).unwrap();
        let provider = catalog.provider(&ProviderId::from("bedrock")).unwrap();
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://bedrock-runtime.eu-west-1.amazonaws.com")
        );
        assert_eq!(provider.auth.as_ref().unwrap().credentials, vec![
            CredentialRef::AwsSigv4
        ]);
        // Bedrock inherits the Anthropic agent profile and billing by default.
        assert_eq!(provider.agent_profile, AgentProfileKind::Anthropic);
        assert_eq!(provider.billing_policy, BillingPolicy::Anthropic);
    }

    #[test]
    fn aws_sigv4_credential_round_trips() {
        assert_eq!(
            "aws_sigv4".parse::<CredentialRef>().unwrap(),
            CredentialRef::AwsSigv4
        );
        assert_eq!(CredentialRef::AwsSigv4.to_string(), "aws_sigv4");
    }

    // ---- Catalog struct tests ----

    #[test]
    fn from_builtin_matches_builtin_catalog() {
        let catalog = Catalog::from_builtin().expect("built-in catalog should build");

        assert_eq!(
            catalog.get("sonnet").map(|model| model.id.as_str()),
            Catalog::builtin()
                .get("sonnet")
                .map(|model| model.id.as_str())
        );
        assert_eq!(
            catalog.default_model().id,
            Catalog::builtin().default_model().id
        );
    }

    #[test]
    fn builtin_overrides_sparse_provider_fields() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.anthropic]
enabled = false
",
        ))
        .expect("sparse built-in provider override should build");

        assert!(catalog.provider(&ProviderId::anthropic()).is_none());
        assert!(catalog.get("claude-sonnet-4-5").is_none());
        assert!(
            catalog
                .providers()
                .iter()
                .any(|provider| provider.id == ProviderId::openai())
        );
    }

    #[test]
    fn builtin_overrides_add_custom_openai_compatible_provider_and_model() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r#"
[providers.acme]
display_name = "Acme"
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "https://api.acme.test/v1"
priority = 120
aliases = ["acme-ai"]

[providers.acme.auth]
credentials = ["env:ACME_API_KEY"]

[models."acme-large"]
provider = "acme"
display_name = "Acme Large"
family = "acme"
default = true
aliases = ["al"]

[models."acme-large".limits]
context_window = 128000

[models."acme-large".features]
tools = true
vision = false
reasoning = false
"#,
        ))
        .expect("custom provider overlay should build");

        let provider = catalog
            .provider(&ProviderId::new("acme-ai"))
            .expect("provider alias should resolve");
        assert_eq!(provider.id, ProviderId::new("acme"));
        assert_eq!(provider.adapter, AdapterKind::OpenAiCompatible);

        let model = catalog.get("al").expect("model alias should resolve");
        assert_eq!(model.id, "acme-large");
        assert_eq!(model.provider, ProviderId::new("acme"));
    }

    #[test]
    fn builtin_bedrock_provider_is_opt_in() {
        let bedrock = ProviderId::new("bedrock");
        let builtin = Catalog::builtin();

        assert!(builtin.provider(&bedrock).is_none());
        assert!(builtin.list(Some(&bedrock)).is_empty());

        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.bedrock]
enabled = true
",
        ))
        .expect("enabled Bedrock override should build from the built-in provider settings");

        let provider = catalog
            .provider(&bedrock)
            .expect("enabled Bedrock provider should be present");
        assert_eq!(provider.adapter, AdapterKind::Bedrock);
        assert_eq!(provider.codec, CodecKind::BedrockConverse);
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://bedrock-runtime.us-east-1.amazonaws.com")
        );
        // Bearer key first (env then vault, like every other provider), under
        // either the AWS-canonical name or Fabro's `<PROVIDER>_API_KEY`
        // convention; SigV4 chain as the fallback.
        assert_eq!(provider.auth.as_ref().unwrap().credentials, vec![
            CredentialRef::Env("AWS_BEARER_TOKEN_BEDROCK".to_string()),
            CredentialRef::Env("BEDROCK_API_KEY".to_string()),
            CredentialRef::Vault("AWS_BEARER_TOKEN_BEDROCK".to_string()),
            CredentialRef::Vault("BEDROCK_API_KEY".to_string()),
            CredentialRef::AwsSigv4,
        ]);

        // Claude rows bill Anthropic-style; open-weights rows override the
        // provider's Anthropic defaults the other way.
        assert_eq!(
            catalog
                .model_settings_on_provider(&bedrock, "claude-sonnet-4-6")
                .unwrap()
                .billing_policy,
            BillingPolicy::Anthropic
        );
        assert_eq!(
            catalog
                .model_settings_on_provider(&bedrock, "glm-5")
                .unwrap()
                .billing_policy,
            BillingPolicy::OpenAi
        );
        assert_eq!(
            catalog
                .model_settings_on_provider(&bedrock, "claude-haiku-4-5")
                .unwrap()
                .api_id,
            "us.anthropic.claude-haiku-4-5-20251001-v1:0"
        );
        assert_eq!(
            catalog
                .default_for_provider(&bedrock)
                .map(|model| model.id.as_str()),
            Some("claude-sonnet-5")
        );
        // Fable 5 ships with sampling params pinned off (the Converse
        // encoder drops temperature/top_p for it).
        let fable = catalog
            .get_on_provider(&bedrock, "claude-fable-5")
            .expect("fable row should be present");
        assert!(!fable.features.sampling_params);
        let fable_settings = catalog
            .settings_for(fable)
            .expect("fable settings should be present");
        assert!(fable_settings.reasoning_by_default);
        assert_eq!(fable_settings.agent_profile, AgentProfileKind::Claude5);
        assert_eq!(
            catalog
                .model_settings_on_provider(&bedrock, "claude-fable-5")
                .unwrap()
                .billing_policy,
            BillingPolicy::Anthropic
        );
        let sonnet = catalog
            .get_on_provider(&bedrock, "claude-sonnet-5")
            .expect("Sonnet 5 row should be present");
        assert_eq!(sonnet.limits.context_window, 1_000_000);
        assert_eq!(sonnet.limits.max_output, Some(128_000));
        assert!(!sonnet.features.sampling_params);
        assert_eq!(
            catalog.settings_for(sonnet).unwrap().agent_profile,
            AgentProfileKind::Claude5
        );
    }

    #[test]
    fn builtin_bedrock_openai_provider_is_opt_in() {
        let provider_id = ProviderId::new("bedrock-openai");
        let builtin = Catalog::builtin();

        assert!(builtin.provider(&provider_id).is_none());

        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.bedrock-openai]
enabled = true
",
        ))
        .expect("enabled bedrock-openai override should build");

        let provider = catalog
            .provider(&provider_id)
            .expect("enabled bedrock-openai provider should be present");
        // OpenAI frontier on Bedrock rides the existing openai_responses
        // dialect against the bedrock-mantle endpoint — pure configuration.
        assert_eq!(provider.adapter, AdapterKind::OpenAi);
        assert_eq!(provider.codec, CodecKind::OpenAiResponses);
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://bedrock-mantle.us-east-1.api.aws/openai/v1")
        );
        assert_eq!(
            catalog
                .default_for_provider(&provider_id)
                .map(|model| model.id.as_str()),
            Some("gpt-5.5")
        );
    }

    #[test]
    fn builtin_poolside_provider_routes_current_laguna_models() {
        let poolside = ProviderId::new("poolside");
        let catalog = Catalog::builtin();
        let provider = catalog
            .provider(&poolside)
            .expect("Poolside provider should be active");

        assert_eq!(provider.adapter, AdapterKind::OpenAiCompatible);
        assert_eq!(provider.codec, CodecKind::OpenAiCompatible);
        assert_eq!(provider.billing_policy, BillingPolicy::OpenAi);
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://inference.poolside.ai/v1")
        );
        assert_eq!(provider.priority, 65);
        assert_eq!(provider.auth.as_ref().unwrap().credentials, vec![
            CredentialRef::Env("POOLSIDE_API_KEY".to_string()),
            CredentialRef::Vault("POOLSIDE_API_KEY".to_string()),
        ]);

        assert_eq!(
            catalog
                .default_for_provider(&poolside)
                .map(|model| model.id.as_str()),
            Some("laguna-s-2.1")
        );
        assert_eq!(
            catalog
                .small_default_for_provider(&poolside)
                .map(|model| model.id.as_str()),
            Some("laguna-xs-2.1")
        );
        assert_eq!(
            catalog
                .probe_for_provider(&poolside)
                .map(|model| model.id.as_str()),
            Some("laguna-xs-2.1")
        );

        let s = catalog.get("laguna").expect("Laguna alias should resolve");
        assert_eq!(s.id, "laguna-s-2.1");
        assert_eq!(s.limits.context_window, 1_048_576);
        assert_eq!(s.limits.max_output, Some(131_072));
        assert!(s.features.tools);
        assert!(s.features.reasoning);
        assert!(s.features.prompt_cache);
        assert!(s.features.sampling_params);
        assert!(!s.features.vision);
        assert!(!s.supports_reasoning_effort());
        assert_eq!(s.costs.input_cost_per_mtok, Some(0.10));
        assert_eq!(s.costs.output_cost_per_mtok, Some(0.20));
        assert_eq!(s.costs.cache_input_cost_per_mtok, Some(0.01));
        assert_eq!(
            catalog.model_settings(&s.id).unwrap().api_id,
            "poolside/laguna-s-2.1"
        );

        let xs = catalog
            .get("laguna-xs")
            .expect("Laguna XS alias should resolve");
        assert_eq!(xs.id, "laguna-xs-2.1");
        assert_eq!(xs.limits.context_window, 262_144);
        assert_eq!(xs.limits.max_output, Some(32_768));
        assert!(xs.features.tools);
        assert!(xs.features.reasoning);
        assert!(xs.features.prompt_cache);
        assert!(xs.features.sampling_params);
        assert!(!xs.features.vision);
        assert!(!xs.supports_reasoning_effort());
        assert_eq!(xs.costs.input_cost_per_mtok, Some(0.10));
        assert_eq!(xs.costs.output_cost_per_mtok, Some(0.20));
        assert_eq!(xs.costs.cache_input_cost_per_mtok, Some(0.05));
        assert_eq!(
            catalog.model_settings(&xs.id).unwrap().api_id,
            "poolside/laguna-xs-2.1"
        );
    }

    #[test]
    fn builtin_deepseek_provider_routes_v4_models() {
        let deepseek = ProviderId::new("deepseek");
        let catalog = Catalog::builtin();
        let provider = catalog
            .provider(&deepseek)
            .expect("DeepSeek provider should be active");

        assert_eq!(provider.adapter, AdapterKind::OpenAiCompatible);
        assert_eq!(provider.codec, CodecKind::OpenAiCompatible);
        assert_eq!(provider.billing_policy, BillingPolicy::OpenAi);
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://api.deepseek.com")
        );
        assert_eq!(provider.priority, 75);
        assert_eq!(provider.auth.as_ref().unwrap().credentials, vec![
            CredentialRef::Env("DEEPSEEK_API_KEY".to_string()),
            CredentialRef::Vault("DEEPSEEK_API_KEY".to_string()),
        ]);
        assert_eq!(
            catalog
                .default_for_provider(&deepseek)
                .map(|model| model.id.as_str()),
            Some("deepseek-v4-flash")
        );
        assert_eq!(
            catalog
                .small_default_for_provider(&deepseek)
                .map(|model| model.id.as_str()),
            Some("deepseek-v4-flash")
        );
        assert_eq!(
            catalog
                .probe_for_provider(&deepseek)
                .map(|model| model.id.as_str()),
            Some("deepseek-v4-flash")
        );

        let expected = [
            ("deepseek-v4-flash", 0.14, 0.28, 0.0028),
            ("deepseek-v4-pro", 0.435, 0.87, 0.003_625),
        ];
        for (id, input, output, cache_read) in expected {
            let model = catalog
                .get_on_provider(&deepseek, id)
                .unwrap_or_else(|| panic!("DeepSeek model '{id}' should be present"));
            assert_eq!(model.family, "deepseek-v4", "{id}");
            assert_eq!(model.limits.context_window, 1_048_576, "{id}");
            assert_eq!(model.limits.max_output, Some(384_000), "{id}");
            assert!(model.features.tools, "{id}");
            assert!(!model.features.vision, "{id}");
            assert!(model.features.reasoning, "{id}");
            assert!(model.features.prompt_cache, "{id}");
            assert!(!model.features.sampling_params, "{id}");
            assert_eq!(model.costs.input_cost_per_mtok, Some(input), "{id}");
            assert_eq!(model.costs.output_cost_per_mtok, Some(output), "{id}");
            assert_eq!(
                model.costs.cache_input_cost_per_mtok,
                Some(cache_read),
                "{id}"
            );

            let settings = catalog
                .model_settings_on_provider(&deepseek, id)
                .unwrap_or_else(|| panic!("DeepSeek settings for '{id}' should be present"));
            assert_eq!(settings.api_id, id, "{id}");
            assert!(settings.reasoning_by_default, "{id}");
        }
    }

    #[test]
    fn builtin_deepseek_reasoning_controls_match_provider_dialects() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.fireworks]
enabled = true

[providers.openrouter]
enabled = true
",
        ))
        .expect("DeepSeek gateway providers should build when enabled");

        let expected = [
            (ProviderId::new("deepseek"), "deepseek-v4-flash", vec![
                ReasoningEffort::Low,
                ReasoningEffort::High,
                ReasoningEffort::Max,
            ]),
            (ProviderId::new("deepseek"), "deepseek-v4-pro", vec![
                ReasoningEffort::High,
                ReasoningEffort::Max,
            ]),
            (ProviderId::new("fireworks"), "deepseek-v4-flash", vec![
                ReasoningEffort::High,
                ReasoningEffort::Max,
            ]),
            (ProviderId::new("fireworks"), "deepseek-v4-pro", vec![
                ReasoningEffort::High,
                ReasoningEffort::Max,
            ]),
            (ProviderId::new("openrouter"), "deepseek-v4-flash", vec![
                ReasoningEffort::Low,
                ReasoningEffort::High,
                ReasoningEffort::Max,
            ]),
            (ProviderId::new("openrouter"), "deepseek-v4-pro", vec![
                ReasoningEffort::High,
                ReasoningEffort::XHigh,
            ]),
        ];

        for (provider, id, efforts) in expected {
            let model = catalog
                .get_on_provider(&provider, id)
                .unwrap_or_else(|| panic!("{provider}/{id} should be present"));
            assert!(model.features.reasoning, "{provider}/{id}");
            assert_eq!(
                model.features.reasoning_effort,
                ReasoningEffortFeature::Levels,
                "{provider}/{id}"
            );
            assert_eq!(model.controls.reasoning_effort, efforts, "{provider}/{id}");
            assert!(!model.features.sampling_params, "{provider}/{id}");

            let settings = catalog
                .model_settings_on_provider(&provider, id)
                .unwrap_or_else(|| panic!("{provider}/{id} settings should be present"));
            assert_eq!(
                settings.agent_profile,
                AgentProfileKind::OpenAi,
                "{provider}/{id}"
            );
            assert!(settings.reasoning_by_default, "{provider}/{id}");
            assert_eq!(
                settings.controls.reasoning_effort, efforts,
                "{provider}/{id}"
            );
        }
    }

    #[test]
    fn builtin_openrouter_provider_is_opt_in() {
        let openrouter = ProviderId::new("openrouter");
        let builtin = Catalog::builtin();

        assert!(builtin.provider(&openrouter).is_none());
        assert!(builtin.list(Some(&openrouter)).is_empty());

        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.openrouter]
enabled = true
",
        ))
        .expect("enabled OpenRouter override should build from the built-in provider settings");

        let provider = catalog
            .provider(&openrouter)
            .expect("enabled OpenRouter provider should be present");
        assert_eq!(provider.adapter, AdapterKind::OpenAiCompatible);
        assert_eq!(provider.codec, CodecKind::OpenAiCompatible);
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(provider.billing_policy, BillingPolicy::OpenAi);

        // Claude rows override the provider's OpenAI billing default;
        // open-weights rows inherit it.
        assert_eq!(
            catalog
                .model_settings_on_provider(&openrouter, "claude-sonnet-5")
                .unwrap()
                .billing_policy,
            BillingPolicy::Anthropic
        );
        assert_eq!(
            catalog
                .model_settings_on_provider(&openrouter, "deepseek-v4-flash")
                .unwrap()
                .billing_policy,
            BillingPolicy::OpenAi
        );
        let deepseek = catalog
            .get_on_provider(&openrouter, "deepseek-v4-flash")
            .expect("DeepSeek V4 Flash should be present on OpenRouter");
        assert_eq!(deepseek.limits.max_output, Some(384_000));
        assert!(deepseek.features.prompt_cache);
        assert_eq!(deepseek.costs.input_cost_per_mtok, Some(0.14));
        assert_eq!(deepseek.costs.output_cost_per_mtok, Some(0.28));
        assert_eq!(deepseek.costs.cache_input_cost_per_mtok, Some(0.0028));
        assert_eq!(
            catalog.settings_for(deepseek).unwrap().api_id,
            "deepseek/deepseek-v4-flash-0731"
        );
        let deepseek_pro = catalog
            .get_on_provider(&openrouter, "deepseek-v4-pro")
            .expect("DeepSeek V4 Pro should be present on OpenRouter");
        assert_eq!(deepseek_pro.limits.max_output, Some(384_000));
        assert!(deepseek_pro.features.prompt_cache);
        assert_eq!(
            deepseek_pro.costs.cache_input_cost_per_mtok,
            Some(0.003_625)
        );
        assert_eq!(
            catalog
                .default_for_provider(&openrouter)
                .map(|model| model.id.as_str()),
            Some("claude-sonnet-5")
        );
    }

    #[test]
    fn builtin_openrouter_includes_gpt_5_6_and_current_claude_models_when_enabled() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.openrouter]
enabled = true
",
        ))
        .expect("enabled OpenRouter override should build from the built-in provider settings");

        let expected = [
            (
                "gpt-5.6-sol",
                "openai/gpt-5.6-sol",
                "gpt-5",
                1_050_000,
                5.0,
                30.0,
                0.5,
                ReasoningEffortFeature::Levels,
                false,
                false,
                BillingPolicy::OpenAi,
            ),
            (
                "gpt-5.6-terra",
                "openai/gpt-5.6-terra",
                "gpt-5",
                1_050_000,
                2.5,
                15.0,
                0.25,
                ReasoningEffortFeature::Levels,
                false,
                false,
                BillingPolicy::OpenAi,
            ),
            (
                "gpt-5.6-luna",
                "openai/gpt-5.6-luna",
                "gpt-5",
                1_050_000,
                1.0,
                6.0,
                0.1,
                ReasoningEffortFeature::Levels,
                false,
                false,
                BillingPolicy::OpenAi,
            ),
            (
                "claude-opus-5",
                "anthropic/claude-opus-5",
                "claude-5",
                1_000_000,
                5.0,
                25.0,
                0.5,
                ReasoningEffortFeature::Levels,
                false,
                true,
                BillingPolicy::Anthropic,
            ),
            (
                "claude-opus-4-8",
                "anthropic/claude-opus-4.8",
                "claude-4",
                1_000_000,
                5.0,
                25.0,
                0.5,
                ReasoningEffortFeature::Levels,
                false,
                true,
                BillingPolicy::Anthropic,
            ),
            (
                "claude-fable-5",
                "anthropic/claude-fable-5",
                "claude-5",
                1_000_000,
                10.0,
                50.0,
                1.0,
                ReasoningEffortFeature::AlwaysAdaptive,
                false,
                true,
                BillingPolicy::Anthropic,
            ),
            (
                "claude-sonnet-5",
                "anthropic/claude-sonnet-5",
                "claude-5",
                1_000_000,
                2.0,
                10.0,
                0.2,
                ReasoningEffortFeature::Levels,
                false,
                true,
                BillingPolicy::Anthropic,
            ),
        ];

        for (
            id,
            api_id,
            family,
            context_window,
            input_cost,
            output_cost,
            cache_input_cost,
            reasoning_effort,
            sampling_params,
            cache_control_breakpoints,
            billing_policy,
        ) in expected
        {
            let model = catalog
                .get_on_provider(&ProviderId::new("openrouter"), id)
                .unwrap_or_else(|| panic!("OpenRouter model '{id}' should be present"));
            assert_eq!(model.provider, ProviderId::new("openrouter"), "{id}");
            assert_eq!(model.family, family, "{id}");
            assert_eq!(model.limits.context_window, context_window, "{id}");
            assert_eq!(model.limits.max_output, Some(128_000), "{id}");
            assert!(model.features.tools, "{id}");
            assert!(model.features.vision, "{id}");
            assert!(model.features.reasoning, "{id}");
            assert!(model.features.prompt_cache, "{id}");
            assert_eq!(model.features.reasoning_effort, reasoning_effort, "{id}");
            assert_eq!(model.features.sampling_params, sampling_params, "{id}");
            assert_eq!(
                model.features.cache_control_breakpoints, cache_control_breakpoints,
                "{id}"
            );
            assert_eq!(model.costs.input_cost_per_mtok, Some(input_cost), "{id}");
            assert_eq!(model.costs.output_cost_per_mtok, Some(output_cost), "{id}");
            assert_eq!(
                model.costs.cache_input_cost_per_mtok,
                Some(cache_input_cost),
                "{id}"
            );

            let settings = catalog
                .model_settings_on_provider(&ProviderId::new("openrouter"), id)
                .unwrap_or_else(|| panic!("OpenRouter settings for '{id}' should be present"));
            assert_eq!(settings.api_id, api_id, "{id}");
            assert_eq!(settings.billing_policy, billing_policy, "{id}");
            assert_eq!(
                settings.controls.reasoning_effort,
                ReasoningEffort::VARIANTS,
                "{id}"
            );
            if family == "claude-5" {
                assert_eq!(settings.agent_profile, AgentProfileKind::Claude5, "{id}");
            }
        }

        for (alias, expected) in [
            ("opus", "claude-opus-5"),
            ("claude-opus", "claude-opus-5"),
            ("sonnet", "claude-sonnet-5"),
            ("claude-sonnet", "claude-sonnet-5"),
        ] {
            let model = catalog
                .resolve_on_provider(&ProviderId::new("openrouter"), alias)
                .unwrap_or_else(|error| panic!("{alias} should resolve on OpenRouter: {error}"));
            assert_eq!(model.id, expected, "{alias}");
        }
    }

    #[test]
    fn builtin_gpt_5_6_short_aliases_are_portable() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.openrouter]
enabled = true
",
        ))
        .expect("enabled OpenRouter override should build from the built-in provider settings");

        for provider in [ProviderId::openai(), ProviderId::new("openrouter")] {
            for (alias, canonical_id) in [
                ("sol", "gpt-5.6-sol"),
                ("gpt-sol", "gpt-5.6-sol"),
                ("terra", "gpt-5.6-terra"),
                ("gpt-terra", "gpt-5.6-terra"),
                ("luna", "gpt-5.6-luna"),
                ("gpt-luna", "gpt-5.6-luna"),
            ] {
                let model = catalog
                    .resolve_on_provider(&provider, alias)
                    .unwrap_or_else(|error| {
                        panic!("{alias} should resolve on {provider}: {error}")
                    });
                assert_eq!(model.provider, provider, "{alias}");
                assert_eq!(model.id, canonical_id, "{alias}");
            }
        }
    }

    #[test]
    fn builtin_glm_aliases_are_portable() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.openrouter]
enabled = true
",
        ))
        .expect("enabled OpenRouter override should build from the built-in provider settings");

        for provider in [ProviderId::new("zai"), ProviderId::new("openrouter")] {
            for (alias, canonical_id) in [
                ("glm", "glm-5.3"),
                ("glm5", "glm-5.3"),
                ("glm53", "glm-5.3"),
                ("glm5.3", "glm-5.3"),
                ("z-ai/glm-5.3", "glm-5.3"),
                ("z-ai/glm-5.3-flash", "glm-5.3-flash"),
                ("glm52", "glm-5.2"),
                ("glm5.2", "glm-5.2"),
            ] {
                let model = catalog
                    .resolve_on_provider(&provider, alias)
                    .unwrap_or_else(|error| {
                        panic!("{alias} should resolve on {provider}: {error}")
                    });
                assert_eq!(model.provider, provider, "{alias}");
                assert_eq!(model.id, canonical_id, "{alias}");
            }
        }
    }

    #[test]
    fn builtin_deepseek_v4_selectors_resolve_on_openrouter() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.openrouter]
enabled = true
",
        ))
        .expect("enabled OpenRouter override should build from the built-in provider settings");
        let openrouter = ProviderId::new("openrouter");

        for (selector, canonical_id) in [
            ("deepseek-v4-pro", "deepseek-v4-pro"),
            ("deepseek-v4", "deepseek-v4-flash"),
            ("deepseek", "deepseek-v4-flash"),
            ("deepseek-v4-flash", "deepseek-v4-flash"),
            ("deepseek-flash", "deepseek-v4-flash"),
        ] {
            let model = catalog
                .resolve_on_provider(&openrouter, selector)
                .unwrap_or_else(|error| {
                    panic!("{selector} should resolve on {openrouter}: {error}")
                });
            assert_eq!(model.provider, openrouter, "{selector}");
            assert_eq!(model.id, canonical_id, "{selector}");
        }
    }

    #[test]
    fn builtin_legacy_vendor_ids_normalize_for_pinned_and_unpinned_selection() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.openrouter]
enabled = true
",
        ))
        .expect("enabled OpenRouter override should build from the built-in provider settings");
        let openrouter = ProviderId::new("openrouter");

        for (selector, canonical_id) in [
            ("anthropic/claude-fable-5", "claude-fable-5"),
            ("openai/gpt-5.6-sol", "gpt-5.6-sol"),
        ] {
            let model = catalog
                .resolve_on_provider(&openrouter, selector)
                .unwrap_or_else(|error| panic!("{selector} should resolve on OpenRouter: {error}"));
            assert_eq!(model.provider, openrouter, "{selector}");
            assert_eq!(model.id, canonical_id, "{selector}");
        }

        let anthropic = ProviderId::anthropic();
        let selector = "anthropic/claude-fable-5";
        let selected = catalog
            .resolve_selection(
                Some(selector),
                None,
                &HashSet::from([anthropic.clone(), openrouter.clone()]),
            )
            .unwrap();
        assert_eq!(selected.provider, anthropic);
        assert_eq!(selected.model, "claude-fable-5");

        let selected = catalog
            .resolve_selection(Some(selector), None, &HashSet::from([openrouter.clone()]))
            .unwrap();
        assert_eq!(selected.provider, openrouter);
        assert_eq!(selected.model, "claude-fable-5");
    }

    #[test]
    fn every_legacy_builtin_identifier_targets_an_existing_offering() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.bedrock]
enabled = true

[providers.bedrock-openai]
enabled = true

[providers.openrouter]
enabled = true
",
        ))
        .expect("all providers referenced by the legacy table should build");

        for (legacy_id, provider_id, canonical_id) in LEGACY_BUILTIN_MODEL_IDENTIFIERS {
            let provider = ProviderId::new(*provider_id);
            let model = catalog
                .resolve_on_provider(&provider, legacy_id)
                .unwrap_or_else(|error| {
                    panic!(
                        "legacy identifier '{legacy_id}' should resolve on '{provider}': {error}"
                    )
                });

            assert_eq!(model.provider, provider, "{legacy_id}");
            assert_eq!(model.id, *canonical_id, "{legacy_id}");
            assert_eq!(
                legacy_builtin_model(legacy_id),
                Some((provider, ModelId::new(*canonical_id))),
                "{legacy_id}"
            );
        }
    }

    #[test]
    fn builtin_openrouter_includes_glm_5_2_when_enabled() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.openrouter]
enabled = true
",
        ))
        .expect("enabled OpenRouter override should build from the built-in provider settings");

        let model = catalog
            .get_on_provider(&ProviderId::new("openrouter"), "glm-5.2")
            .expect("OpenRouter GLM 5.2 should be present");
        insta::assert_debug_snapshot!(model, @r#"
        Model {
            id: "glm-5.2",
            provider: openrouter,
            family: "glm-5",
            display_name: "GLM 5.2 (via OpenRouter)",
            limits: ModelLimits {
                context_window: 1048576,
                max_output: Some(
                    131072,
                ),
            },
            training: None,
            knowledge_cutoff: None,
            features: ModelFeatures {
                tools: true,
                vision: false,
                reasoning: true,
                reasoning_effort: Levels,
                prompt_cache: true,
                cache_control_breakpoints: false,
                sampling_params: true,
            },
            controls: ModelControls {
                reasoning_effort: [
                    High,
                    XHigh,
                ],
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(
                    0.784,
                ),
                output_cost_per_mtok: Some(
                    2.464,
                ),
                cache_input_cost_per_mtok: Some(
                    0.1456,
                ),
            },
            estimated_output_tps: None,
            aliases: [
                "glm52",
                "glm5.2",
            ],
            default: false,
            small_default: false,
            configured: false,
        }
        "#);

        let settings = catalog
            .model_settings_on_provider(&ProviderId::new("openrouter"), "glm-5.2")
            .expect("OpenRouter GLM 5.2 settings should be present");
        assert_eq!(settings.api_id, "z-ai/glm-5.2");
        assert_eq!(settings.controls.reasoning_effort, vec![
            ReasoningEffort::High,
            ReasoningEffort::XHigh
        ]);
    }

    #[test]
    fn builtin_openrouter_includes_kimi_k3_when_enabled() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.openrouter]
enabled = true
",
        ))
        .expect("enabled OpenRouter override should build from the built-in provider settings");

        let model = catalog
            .get_on_provider(&ProviderId::new("openrouter"), "kimi-k3")
            .expect("OpenRouter Kimi K3 should be present");
        insta::assert_debug_snapshot!(model, @r#"
        Model {
            id: "kimi-k3",
            provider: openrouter,
            family: "kimi-k3",
            display_name: "Kimi K3 (via OpenRouter)",
            limits: ModelLimits {
                context_window: 1048576,
                max_output: Some(
                    131072,
                ),
            },
            training: None,
            knowledge_cutoff: None,
            features: ModelFeatures {
                tools: true,
                vision: true,
                reasoning: true,
                reasoning_effort: AlwaysAdaptive,
                prompt_cache: true,
                cache_control_breakpoints: false,
                sampling_params: false,
            },
            controls: ModelControls {
                reasoning_effort: [
                    Low,
                    High,
                    Max,
                ],
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(
                    3.0,
                ),
                output_cost_per_mtok: Some(
                    15.0,
                ),
                cache_input_cost_per_mtok: Some(
                    0.3,
                ),
            },
            estimated_output_tps: None,
            aliases: [],
            default: false,
            small_default: false,
            configured: false,
        }
        "#);

        let settings = catalog
            .model_settings_on_provider(&ProviderId::new("openrouter"), "kimi-k3")
            .expect("OpenRouter Kimi K3 settings should be present");
        assert_eq!(settings.api_id, "moonshotai/kimi-k3");
        assert_eq!(settings.controls.reasoning_effort, vec![
            ReasoningEffort::Low,
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ]);
    }

    #[test]
    fn builtin_openrouter_includes_qwen3_8_max_when_enabled() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.openrouter]
enabled = true
",
        ))
        .expect("enabled OpenRouter override should build from the built-in provider settings");

        let model = catalog
            .get_on_provider(&ProviderId::new("openrouter"), "qwen3.8-max")
            .expect("OpenRouter Qwen3.8 Max should be present");
        insta::assert_debug_snapshot!(model, @r#"
        Model {
            id: "qwen3.8-max",
            provider: openrouter,
            family: "qwen3",
            display_name: "Qwen3.8 Max",
            limits: ModelLimits {
                context_window: 1000000,
                max_output: Some(
                    131072,
                ),
            },
            training: None,
            knowledge_cutoff: None,
            features: ModelFeatures {
                tools: true,
                vision: true,
                reasoning: true,
                reasoning_effort: Levels,
                prompt_cache: true,
                cache_control_breakpoints: false,
                sampling_params: true,
            },
            controls: ModelControls {
                reasoning_effort: [
                    Low,
                    Medium,
                    High,
                    XHigh,
                ],
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(
                    2.0,
                ),
                output_cost_per_mtok: Some(
                    6.0,
                ),
                cache_input_cost_per_mtok: Some(
                    0.25,
                ),
            },
            estimated_output_tps: None,
            aliases: [],
            default: false,
            small_default: false,
            configured: false,
        }
        "#);

        let settings = catalog
            .model_settings_on_provider(&ProviderId::new("openrouter"), "qwen3.8-max")
            .expect("OpenRouter Qwen3.8 Max settings should be present");
        assert_eq!(settings.api_id, "qwen/qwen3.8-max");
        assert!(settings.reasoning_by_default);
        assert_eq!(settings.controls.reasoning_effort, vec![
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
        ]);
    }

    #[test]
    fn builtin_modal_provider_is_opt_in() {
        let modal = ProviderId::new("modal");
        let builtin = Catalog::builtin();

        assert!(builtin.provider(&modal).is_none());
        assert!(builtin.list(Some(&modal)).is_empty());

        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.modal]
enabled = true
",
        ))
        .expect("enabled Modal override should build from the built-in provider settings");

        let provider = catalog
            .provider(&modal)
            .expect("enabled Modal provider should be present");
        assert_eq!(provider.adapter, AdapterKind::OpenAiCompatible);
        assert_eq!(provider.codec, CodecKind::OpenAiCompatible);
        assert_eq!(provider.agent_profile, AgentProfileKind::Kimi);
        assert_eq!(provider.billing_policy, BillingPolicy::OpenAi);
        assert_eq!(provider.priority, 75);
        assert!(provider.auth.is_none());
        assert_eq!(
            provider.extra_headers,
            HashMap::from([
                (
                    "Modal-Key".to_string(),
                    "{{ secrets.MODAL_TOKEN_ID }}".to_string(),
                ),
                (
                    "Modal-Secret".to_string(),
                    "{{ secrets.MODAL_TOKEN_SECRET }}".to_string(),
                ),
            ])
        );

        // Modal assigns the endpoint URL per deployment, so the built-in entry
        // ships without one and the operator supplies it through settings.
        assert!(provider.base_url.is_none());
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r#"
[providers.modal]
enabled = true
base_url = "https://example--kimi-k3.modal.run/v1"
"#,
        ))
        .expect("Modal base URL override should build");
        assert_eq!(
            catalog
                .provider(&modal)
                .and_then(|provider| provider.base_url.as_deref()),
            Some("https://example--kimi-k3.modal.run/v1")
        );
    }

    #[test]
    fn builtin_modal_includes_kimi_k3_when_enabled() {
        let modal = ProviderId::new("modal");
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.modal]
enabled = true
",
        ))
        .expect("enabled Modal override should build from the built-in provider settings");

        assert_eq!(catalog.list(Some(&modal)).len(), 1);
        let model = catalog
            .get_on_provider(&modal, "kimi-k3")
            .expect("Modal Kimi K3 should be present");
        insta::assert_debug_snapshot!(model, @r#"
        Model {
            id: "kimi-k3",
            provider: modal,
            family: "kimi-k3",
            display_name: "Kimi K3 (via Modal)",
            limits: ModelLimits {
                context_window: 1048576,
                max_output: Some(
                    131072,
                ),
            },
            training: None,
            knowledge_cutoff: None,
            features: ModelFeatures {
                tools: true,
                vision: true,
                reasoning: true,
                reasoning_effort: AlwaysAdaptive,
                prompt_cache: true,
                cache_control_breakpoints: false,
                sampling_params: false,
            },
            controls: ModelControls {
                reasoning_effort: [
                    Low,
                    High,
                    Max,
                ],
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(
                    3.0,
                ),
                output_cost_per_mtok: Some(
                    15.0,
                ),
                cache_input_cost_per_mtok: Some(
                    0.3,
                ),
            },
            estimated_output_tps: Some(
                460.0,
            ),
            aliases: [],
            default: true,
            small_default: false,
            configured: false,
        }
        "#);

        let settings = catalog
            .model_settings_on_provider(&modal, "kimi-k3")
            .expect("Modal Kimi K3 settings should be present");
        assert_eq!(settings.api_id, "moonshotai/Kimi-K3");
        assert_eq!(settings.agent_profile, AgentProfileKind::Kimi);
        assert_eq!(settings.billing_policy, BillingPolicy::OpenAi);
        assert_eq!(settings.controls.reasoning_effort, vec![
            ReasoningEffort::Low,
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ]);
    }

    #[test]
    fn builtin_moonshot_provider_prefers_moonshot_api_key() {
        let moonshot = ProviderId::new("moonshot");
        let catalog = Catalog::builtin();
        let provider = catalog
            .provider(&moonshot)
            .expect("Moonshot provider should be present");

        assert_eq!(provider.auth.as_ref().unwrap().credentials, vec![
            CredentialRef::Env("MOONSHOT_API_KEY".to_string()),
            CredentialRef::Env("KIMI_API_KEY".to_string()),
            CredentialRef::Vault("MOONSHOT_API_KEY".to_string()),
            CredentialRef::Vault("KIMI_API_KEY".to_string()),
        ]);
        assert_eq!(
            catalog.provider_vault_secret_name(&moonshot),
            Some("MOONSHOT_API_KEY")
        );
    }

    #[test]
    fn builtin_kimi_k3_selection_follows_provider_priority() {
        let moonshot = ProviderId::new("moonshot");
        let modal = ProviderId::new("modal");
        let fireworks = ProviderId::new("fireworks");
        let openrouter = ProviderId::new("openrouter");
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.modal]
enabled = true

[providers.fireworks]
enabled = true

[providers.openrouter]
enabled = true
",
        ))
        .expect("enabled Kimi K3 provider overrides should build");

        let selected = catalog
            .select(
                "kimi-k3",
                None,
                &HashSet::from([
                    moonshot.clone(),
                    modal.clone(),
                    fireworks.clone(),
                    openrouter.clone(),
                ]),
            )
            .expect("Modal should win portable Kimi K3 selection");
        assert_eq!(selected.provider, modal);

        let selected = catalog
            .select(
                "kimi-k3",
                None,
                &HashSet::from([moonshot.clone(), fireworks.clone(), openrouter.clone()]),
            )
            .expect("Moonshot should win when Modal is unavailable");
        assert_eq!(selected.provider, moonshot);

        let selected = catalog
            .select(
                "kimi-k3",
                None,
                &HashSet::from([fireworks.clone(), openrouter]),
            )
            .expect("Fireworks should win when only gateway routes are available");
        assert_eq!(selected.provider, fireworks);
    }

    #[test]
    fn builtin_openrouter_includes_poolside_laguna_when_enabled() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.openrouter]
enabled = true
",
        ))
        .expect("enabled OpenRouter override should build from the built-in provider settings");

        let expected = [
            ("laguna-s-2.1", 1_048_576, 131_072, 0.10, 0.20, 0.01),
            ("laguna-xs-2.1", 262_144, 32_768, 0.06, 0.12, 0.03),
        ];

        for (id, context, max_output, input, output, cache_read) in expected {
            let model = catalog
                .get_on_provider(&ProviderId::new("openrouter"), id)
                .unwrap_or_else(|| panic!("OpenRouter model '{id}' should be present"));
            assert_eq!(model.provider, ProviderId::new("openrouter"), "{id}");
            assert_eq!(model.family, "laguna-2", "{id}");
            assert_eq!(model.limits.context_window, context, "{id}");
            assert_eq!(model.limits.max_output, Some(max_output), "{id}");
            assert!(model.features.tools, "{id}");
            assert!(model.features.reasoning, "{id}");
            assert!(model.features.prompt_cache, "{id}");
            assert!(model.features.sampling_params, "{id}");
            assert!(!model.features.vision, "{id}");
            assert!(!model.supports_reasoning_effort(), "{id}");
            assert_eq!(model.costs.input_cost_per_mtok, Some(input), "{id}");
            assert_eq!(model.costs.output_cost_per_mtok, Some(output), "{id}");
            assert_eq!(
                model.costs.cache_input_cost_per_mtok,
                Some(cache_read),
                "{id}"
            );

            let settings = catalog
                .model_settings_on_provider(&ProviderId::new("openrouter"), id)
                .unwrap_or_else(|| panic!("OpenRouter settings for '{id}' should be present"));
            assert_eq!(settings.api_id, format!("poolside/{id}"), "{id}");
            assert!(settings.controls.reasoning_effort.is_empty(), "{id}");
        }
    }

    #[test]
    fn builtin_fireworks_provider_is_opt_in() {
        let fireworks = ProviderId::new("fireworks");
        let builtin = Catalog::builtin();

        assert!(builtin.provider(&fireworks).is_none());
        assert!(builtin.list(Some(&fireworks)).is_empty());

        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.fireworks]
enabled = true
",
        ))
        .expect("enabled Fireworks override should build from the built-in provider settings");

        let provider = catalog
            .provider(&fireworks)
            .expect("enabled Fireworks provider should be present");
        assert_eq!(provider.adapter, AdapterKind::OpenAiCompatible);
        assert_eq!(provider.codec, CodecKind::OpenAiCompatible);
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://api.fireworks.ai/inference/v1")
        );
        assert_eq!(provider.billing_policy, BillingPolicy::OpenAi);
        assert_eq!(provider.priority, 30);
        assert_eq!(provider.auth.as_ref().unwrap().credentials, vec![
            CredentialRef::Env("FIREWORKS_API_KEY".to_string()),
            CredentialRef::Vault("FIREWORKS_API_KEY".to_string()),
        ]);

        assert_eq!(
            catalog
                .default_for_provider(&fireworks)
                .map(|model| model.id.as_str()),
            Some("kimi-k2.7-code")
        );
        assert_eq!(
            catalog
                .small_default_for_provider(&fireworks)
                .map(|model| model.id.as_str()),
            Some("gpt-oss-20b")
        );
        assert_eq!(
            catalog
                .probe_for_provider(&fireworks)
                .map(|model| model.id.as_str()),
            Some("gpt-oss-20b")
        );
    }

    #[test]
    fn builtin_fireworks_models_when_enabled() {
        let fireworks = ProviderId::new("fireworks");
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.fireworks]
enabled = true
",
        ))
        .expect("enabled Fireworks override should build from the built-in provider settings");

        // (id, api_id, family, context_window, max_output, vision, reasoning,
        //  input, output, cache_read)
        let expected = [
            (
                "kimi-k3",
                "accounts/fireworks/models/kimi-k3",
                "kimi-k3",
                1_048_576,
                131_072,
                true,
                true,
                3.0,
                15.0,
                0.3,
            ),
            (
                "kimi-k3-fast",
                "accounts/fireworks/routers/kimi-k3-fast",
                "kimi-k3",
                1_048_576,
                131_072,
                true,
                true,
                4.5,
                22.5,
                0.45,
            ),
            (
                "kimi-k2.7-code",
                "accounts/fireworks/models/kimi-k2p7-code",
                "kimi-k2",
                262_144,
                32_768,
                true,
                true,
                0.95,
                4.0,
                0.19,
            ),
            (
                "kimi-k2.6",
                "accounts/fireworks/models/kimi-k2p6",
                "kimi-k2",
                262_144,
                16_384,
                false,
                false,
                0.95,
                4.0,
                0.16,
            ),
            (
                "deepseek-v4-pro",
                "accounts/fireworks/models/deepseek-v4-pro",
                "deepseek-v4",
                1_048_576,
                16_384,
                false,
                true,
                1.74,
                3.48,
                0.145,
            ),
            (
                "deepseek-v4-flash",
                "accounts/fireworks/models/deepseek-v4-flash",
                "deepseek-v4",
                1_048_576,
                384_000,
                false,
                true,
                0.14,
                0.28,
                0.028,
            ),
            (
                "glm-5.3",
                "accounts/fireworks/models/glm-5p3",
                "glm-5",
                1_048_576,
                131_072,
                false,
                true,
                1.4,
                4.4,
                0.26,
            ),
            (
                "glm-5.3-flash",
                "accounts/fireworks/models/glm-5p3-flash",
                "glm-5",
                1_048_576,
                131_072,
                true,
                true,
                0.15,
                0.5,
                0.03,
            ),
            (
                "glm-5.2",
                "accounts/fireworks/models/glm-5p2",
                "glm-5",
                1_048_576,
                131_072,
                false,
                true,
                1.4,
                4.4,
                0.14,
            ),
            (
                "minimax-m2.7",
                "accounts/fireworks/models/minimax-m2p7",
                "minimax-m2",
                196_608,
                16_384,
                false,
                false,
                0.3,
                1.2,
                0.059,
            ),
            (
                "qwen3.7-plus",
                "accounts/fireworks/models/qwen3p7-plus",
                "qwen3",
                262_144,
                16_384,
                true,
                false,
                0.4,
                1.6,
                0.08,
            ),
            (
                "gpt-oss-120b",
                "accounts/fireworks/models/gpt-oss-120b",
                "gpt-oss",
                131_072,
                32_768,
                false,
                true,
                0.15,
                0.6,
                0.015,
            ),
            (
                "gpt-oss-20b",
                "accounts/fireworks/models/gpt-oss-20b",
                "gpt-oss",
                131_072,
                32_768,
                false,
                true,
                0.07,
                0.3,
                0.035,
            ),
        ];

        let mut model_ids: Vec<&str> = catalog
            .list(Some(&fireworks))
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        model_ids.sort_unstable();
        let mut expected_ids: Vec<&str> = expected.iter().map(|row| row.0).collect();
        expected_ids.sort_unstable();
        assert_eq!(
            model_ids, expected_ids,
            "expected rows must cover every Fireworks model"
        );

        for (
            id,
            api_id,
            family,
            context,
            max_output,
            vision,
            reasoning,
            input,
            output,
            cache_read,
        ) in expected
        {
            let model = catalog
                .get_on_provider(&fireworks, id)
                .unwrap_or_else(|| panic!("Fireworks model '{id}' should be present"));
            assert_eq!(model.family, family, "{id}");
            assert_eq!(model.limits.context_window, context, "{id}");
            assert_eq!(model.limits.max_output, Some(max_output), "{id}");
            assert!(model.features.tools, "{id}");
            assert_eq!(model.features.vision, vision, "{id}");
            assert_eq!(model.features.reasoning, reasoning, "{id}");
            assert!(model.features.prompt_cache, "{id}");
            assert_eq!(model.costs.input_cost_per_mtok, Some(input), "{id}");
            assert_eq!(model.costs.output_cost_per_mtok, Some(output), "{id}");
            assert_eq!(
                model.costs.cache_input_cost_per_mtok,
                Some(cache_read),
                "{id}"
            );

            let settings = catalog
                .model_settings_on_provider(&fireworks, id)
                .unwrap_or_else(|| panic!("Fireworks settings for '{id}' should be present"));
            assert_eq!(settings.api_id, api_id, "{id}");
            assert_eq!(settings.billing_policy, BillingPolicy::OpenAi, "{id}");
        }

        for id in ["kimi-k3", "kimi-k3-fast"] {
            let model = catalog
                .get_on_provider(&fireworks, id)
                .unwrap_or_else(|| panic!("Fireworks model '{id}' should be present"));
            assert_eq!(
                model.features.reasoning_effort,
                ReasoningEffortFeature::AlwaysAdaptive,
                "{id}"
            );
            assert!(!model.features.sampling_params, "{id}");

            let settings = catalog
                .model_settings_on_provider(&fireworks, id)
                .unwrap_or_else(|| panic!("Fireworks settings for '{id}' should be present"));
            assert_eq!(settings.agent_profile, AgentProfileKind::Kimi, "{id}");
            assert_eq!(
                settings.controls.reasoning_effort,
                [
                    ReasoningEffort::Low,
                    ReasoningEffort::Medium,
                    ReasoningEffort::High,
                ],
                "{id}"
            );
        }
    }

    #[test]
    fn builtin_deepseek_shared_slugs_are_portable_across_providers() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.fireworks]
enabled = true

[providers.openrouter]
enabled = true
",
        ))
        .expect("enabled Fireworks and OpenRouter overrides should build");

        for provider in [ProviderId::new("fireworks"), ProviderId::new("openrouter")] {
            for id in [
                "kimi-k3",
                "kimi-k2.6",
                "deepseek-v4-pro",
                "deepseek-v4-flash",
                "glm-5.3",
                "glm-5.3-flash",
                "glm-5.2",
                "minimax-m2.7",
            ] {
                let model = catalog
                    .get_on_provider(&provider, id)
                    .unwrap_or_else(|| panic!("'{id}' should resolve on provider '{provider}'"));
                assert_eq!(model.id, id, "{provider}/{id}");
                assert_eq!(model.provider, provider, "{provider}/{id}");
            }
        }

        for provider in [
            ProviderId::new("deepseek"),
            ProviderId::new("fireworks"),
            ProviderId::new("openrouter"),
        ] {
            for id in ["deepseek-v4-pro", "deepseek-v4-flash"] {
                let model = catalog
                    .get_on_provider(&provider, id)
                    .unwrap_or_else(|| panic!("'{id}' should resolve on provider '{provider}'"));
                assert_eq!(model.id, id, "{provider}/{id}");
                assert_eq!(model.provider, provider, "{provider}/{id}");
                assert_eq!(model.limits.context_window, 1_048_576, "{provider}/{id}");
            }
            for alias in ["deepseek", "deepseek-v4", "deepseek-flash"] {
                let model = catalog
                    .resolve_on_provider(&provider, alias)
                    .unwrap_or_else(|error| {
                        panic!("'{alias}' should resolve on provider '{provider}': {error}")
                    });
                assert_eq!(model.id, "deepseek-v4-flash", "{provider}/{alias}");
                assert_eq!(model.provider, provider, "{provider}/{alias}");
            }
        }

        let selected = catalog
            .select(
                "deepseek",
                None,
                &HashSet::from([
                    ProviderId::new("deepseek"),
                    ProviderId::new("fireworks"),
                    ProviderId::new("openrouter"),
                ]),
            )
            .expect("direct DeepSeek should win portable DeepSeek selection");
        assert_eq!(selected.provider, ProviderId::new("deepseek"));
    }

    #[test]
    fn builtin_ollama_provider_is_opt_in() {
        let ollama = ProviderId::new("ollama");
        let builtin = Catalog::builtin();

        assert!(builtin.provider(&ollama).is_none());
        assert!(builtin.list(Some(&ollama)).is_empty());

        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r"
[providers.ollama]
enabled = true
",
        ))
        .expect("enabled Ollama override should build from the built-in provider settings");

        let provider = catalog
            .provider(&ollama)
            .expect("enabled Ollama provider should be present");
        assert_eq!(provider.adapter, AdapterKind::OpenAiCompatible);
        assert_eq!(
            provider.base_url.as_deref(),
            Some("http://localhost:11434/v1")
        );
        assert_eq!(provider.billing_policy, BillingPolicy::None);

        assert!(catalog.list(Some(&ollama)).is_empty());
        assert!(catalog.default_for_provider(&ollama).is_none());
    }

    #[test]
    fn builtin_get_by_id() {
        let m = Catalog::builtin().get("claude-opus-4-6").unwrap();
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
        let anthropic = Catalog::builtin().list(Some(&ProviderId::anthropic()));
        assert!(!anthropic.is_empty());
        assert!(
            anthropic
                .iter()
                .all(|m| m.provider == ProviderId::anthropic())
        );
    }

    #[test]
    fn builtin_list_unknown_provider_empty() {
        let models = Catalog::builtin().list(Some(&ProviderId::new("missing-provider")));
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
            .default_for_provider(&ProviderId::anthropic())
            .unwrap();
        assert_eq!(m.id, "claude-sonnet-5");
        assert!(m.default);

        let m = Catalog::builtin()
            .default_for_provider(&ProviderId::openai())
            .unwrap();
        assert_eq!(m.provider, ProviderId::openai());
        assert!(m.default);

        let m = Catalog::builtin()
            .default_for_provider(&ProviderId::gemini())
            .unwrap();
        assert_eq!(m.id, "gemini-3.5-flash");
    }

    #[test]
    fn builtin_probe_openai_returns_override() {
        let m = Catalog::builtin()
            .probe_for_provider(&ProviderId::openai())
            .unwrap();
        assert_eq!(m.id, "gpt-5.4-mini");
    }

    #[test]
    fn builtin_probe_anthropic_returns_override() {
        let m = Catalog::builtin()
            .probe_for_provider(&ProviderId::anthropic())
            .unwrap();
        assert_eq!(m.id, "claude-haiku-4-5");
    }

    #[test]
    fn builtin_probe_gemini_returns_default() {
        let m = Catalog::builtin()
            .probe_for_provider(&ProviderId::gemini())
            .unwrap();
        assert_eq!(m.id, "gemini-3.5-flash");
    }

    #[test]
    fn builtin_small_defaults_are_marked_per_provider() {
        let catalog = Catalog::builtin();

        let small_defaults = catalog
            .list(None)
            .into_iter()
            .filter(|model| model.small_default)
            .collect::<Vec<_>>();

        assert!(
            !small_defaults.is_empty(),
            "built-in catalog should mark at least one small default model"
        );

        for model in small_defaults {
            assert_eq!(
                catalog
                    .small_default_for_provider(&model.provider)
                    .unwrap()
                    .id,
                model.id
            );
        }
    }

    #[test]
    fn builtin_closest_opus_to_gemini() {
        let opus = Catalog::builtin().get("claude-opus-4-6").unwrap();
        let result = Catalog::builtin()
            .closest(&ProviderId::gemini(), opus)
            .unwrap();
        assert_eq!(result.id, "gemini-3.1-pro-preview");
    }

    #[test]
    fn builtin_closest_no_match() {
        let haiku = Catalog::builtin().get("claude-haiku-4-5").unwrap();
        assert!(
            Catalog::builtin()
                .closest(&ProviderId::openai(), haiku)
                .is_none()
        );
    }

    #[test]
    fn builtin_catalog_is_loaded_from_provider_toml_settings() {
        let catalog = Catalog::builtin();

        assert_eq!(
            catalog.provider(&ProviderId::openai()).unwrap().adapter,
            AdapterKind::OpenAi
        );
        assert_eq!(
            catalog
                .provider(&ProviderId::openai())
                .unwrap()
                .api_key_url
                .as_deref(),
            Some("https://platform.openai.com/api-keys")
        );
        assert_eq!(
            catalog
                .provider(&ProviderId::new("moonshot"))
                .unwrap()
                .base_url
                .as_deref(),
            Some("https://api.moonshot.ai/v1")
        );
        assert_eq!(catalog.model_settings("gpt-5.4").unwrap().api_id, "gpt-5.4");
        assert_eq!(
            catalog.get("claude-opus-4-7").unwrap().knowledge_cutoff(),
            Some("May 2025")
        );
        assert_eq!(
            catalog
                .model_settings("gpt-5.4")
                .unwrap()
                .controls
                .reasoning_effort,
            ReasoningEffort::VARIANTS
        );
        assert_eq!(
            catalog
                .model_settings("claude-sonnet-4-5")
                .unwrap()
                .controls
                .reasoning_effort,
            ReasoningEffort::VARIANTS
        );
    }

    #[test]
    fn catalog_from_settings_rejects_unknown_adapter() {
        let layer = minimal_settings(
            r#"
[providers.test-provider]
display_name = "Test Provider"
adapter = "not_real"
enabled = true
"#,
        );

        let err = Catalog::from_settings(&layer).unwrap_err();

        assert!(matches!(
            err,
            CatalogBuildError::UnknownAdapter { provider, adapter }
                if provider == ProviderId::new("test-provider") && adapter == "not_real"
        ));
    }

    // ---- Codec on the route ----

    #[test]
    fn provider_codec_defaults_from_adapter() {
        let catalog = Catalog::builtin();

        for (provider, expected) in [
            ("anthropic", CodecKind::AnthropicMessages),
            ("openai", CodecKind::OpenAiResponses),
            ("gemini", CodecKind::GeminiGenerate),
            ("moonshot", CodecKind::OpenAiCompatible),
        ] {
            let provider_id = ProviderId::new(provider);
            assert_eq!(catalog.provider(&provider_id).unwrap().codec, expected);
            assert_eq!(catalog.effective_codec(&provider_id, None), Some(expected));
        }
    }

    #[test]
    fn model_codec_inherits_provider_codec() {
        let catalog = Catalog::builtin();

        assert_eq!(
            catalog.model_settings("claude-sonnet-4-5").unwrap().codec,
            CodecKind::AnthropicMessages
        );
        assert_eq!(
            catalog.model_settings("gpt-5.4").unwrap().codec,
            CodecKind::OpenAiResponses
        );
        assert_eq!(
            catalog.effective_codec(&ProviderId::anthropic(), Some("claude-sonnet-4-5")),
            Some(CodecKind::AnthropicMessages)
        );
    }

    #[test]
    fn explicit_codec_matching_the_adapter_default_is_accepted() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r#"
[providers.acme]
display_name = "Acme"
adapter = "openai_compatible"
codec = "openai_compatible"
base_url = "https://api.acme.test/v1"

[models."acme-large"]
provider = "acme"
codec = "openai_compatible"
display_name = "Acme Large"
family = "acme"

[models."acme-large".limits]
context_window = 128000

[models."acme-large".features]
tools = true
vision = false
reasoning = false
"#,
        ))
        .expect("default codec pairing should build");

        assert_eq!(
            catalog.provider(&ProviderId::new("acme")).unwrap().codec,
            CodecKind::OpenAiCompatible
        );
        assert_eq!(
            catalog.model_settings("acme-large").unwrap().codec,
            CodecKind::OpenAiCompatible
        );
        assert_eq!(
            catalog.effective_codec(&ProviderId::new("acme"), Some("acme-large")),
            Some(CodecKind::OpenAiCompatible)
        );
    }

    #[test]
    fn provider_codec_outside_the_adapter_default_is_rejected() {
        let layer = minimal_settings(
            r#"
[providers.test-provider]
display_name = "Test Provider"
adapter = "openai"
codec = "anthropic_messages"
"#,
        );

        let err = Catalog::from_settings(&layer).unwrap_err();

        assert!(matches!(
            err,
            CatalogBuildError::UnsupportedProviderCodec {
                provider,
                adapter: AdapterKind::OpenAi,
                codec: CodecKind::AnthropicMessages,
                expected: CodecKind::OpenAiResponses,
            } if provider == ProviderId::new("test-provider")
        ));
    }

    #[test]
    fn model_codec_outside_the_adapter_default_is_rejected() {
        let layer = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
enabled = true

[models.one]
provider = "test"
codec = "gemini_generate"
display_name = "One"
family = "test"
default = true

[models.one.limits]
context_window = 1000

[models.one.features]
tools = false
vision = false
reasoning = false
"#,
        );

        let err = Catalog::from_settings(&layer).unwrap_err();

        assert!(matches!(
            err,
            CatalogBuildError::UnsupportedModelCodec {
                model,
                adapter: AdapterKind::OpenAi,
                codec: CodecKind::GeminiGenerate,
                expected: CodecKind::OpenAiResponses,
            } if model == "one"
        ));
    }

    #[test]
    fn builtin_override_can_pin_the_default_codec() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r#"
[providers.anthropic]
codec = "anthropic_messages"
"#,
        ))
        .expect("override pinning the default codec should build");

        assert_eq!(
            catalog.provider(&ProviderId::anthropic()).unwrap().codec,
            CodecKind::AnthropicMessages
        );
    }

    #[test]
    /// Canonical IDs, aliases, and API IDs share one identifier namespace per
    /// provider, so a collision in any of them is rejected the same way.
    fn catalog_from_settings_rejects_duplicate_provider_model_selectors() {
        for (declaration, expected) in [
            (r#"aliases = ["shared"]"#, "shared"),
            (r#"api_id = "vendor/shared""#, "vendor/shared"),
        ] {
            let layer = minimal_settings(&format!(
                r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"
enabled = true

[providers.test.models.one]
display_name = "One"
family = "test"
{declaration}

[providers.test.models.one.limits]
context_window = 1000

[providers.test.models.one.features]
tools = false
vision = false
reasoning = false

[providers.test.models.two]
display_name = "Two"
family = "test"
{declaration}

[providers.test.models.two.limits]
context_window = 1000

[providers.test.models.two.features]
tools = false
vision = false
reasoning = false
"#
            ));

            let err = Catalog::from_settings(&layer).unwrap_err();

            assert!(
                matches!(
                    &err,
                    CatalogBuildError::DuplicateProviderModelSelector {
                        provider,
                        selector,
                        first,
                        second,
                    } if provider == &ProviderId::new("test")
                        && selector == expected
                        && first == "one"
                        && second == "two"
                ),
                "{declaration}: {err:?}"
            );
        }
    }

    #[test]
    fn provider_scoped_lookup_accepts_canonical_alias_and_api_id_selectors() {
        let catalog = Catalog::from_settings(&minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"
aliases = ["test-alias"]

[providers.test.models.one]
api_id = "vendor/models/one:latest"
display_name = "One"
family = "test"
aliases = ["one-alias"]
default = true

[providers.test.models.one.limits]
context_window = 1000

[providers.test.models.one.features]
tools = false
vision = false
reasoning = false
"#,
        ))
        .expect("provider-scoped selector fixture should build");

        for selector in ["one", "one-alias", "vendor/models/one:latest"] {
            let model = catalog
                .resolve_on_provider(&ProviderId::new("test-alias"), selector)
                .unwrap_or_else(|error| {
                    panic!("selector '{selector}' should resolve on provider alias: {error}")
                });
            assert_eq!(model.provider, ProviderId::new("test"), "{selector}");
            assert_eq!(model.id, "one", "{selector}");
        }

        assert!(matches!(
            catalog.select(
                "vendor/models/one:latest",
                None,
                &HashSet::from([ProviderId::new("test")]),
            ),
            Err(ModelSelectionError::UnknownSelector { selector })
                if selector == "vendor/models/one:latest"
        ));
    }

    #[test]
    fn provider_scoped_model_rejects_redundant_provider_field() {
        let error = Catalog::from_settings(&minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"

[providers.test.models.one]
provider = "test"
"#,
        ))
        .unwrap_err();

        assert!(matches!(
            error,
            CatalogBuildError::LegacyModel(LegacyModelError::ScopedModelDeclaresProvider {
                provider,
                model,
            }) if provider == ProviderId::new("test") && model == "one"
        ));
    }

    #[test]
    fn provider_scoped_model_rejects_legacy_builtin_id_as_canonical_id() {
        let error = Catalog::from_settings(&minimal_settings(
            r#"
[providers.openrouter]
display_name = "OpenRouter"
adapter = "openai_compatible"

[providers.openrouter.models."openai/gpt-5.6-sol"]
"#,
        ))
        .unwrap_err();

        assert!(matches!(
            error,
            CatalogBuildError::LegacyModel(
                LegacyModelError::LegacyIdentifierAsModelId {
                    identifier,
                    provider,
                    model,
                }
            ) if identifier == "openai/gpt-5.6-sol"
                && provider == ProviderId::new("openrouter")
                && model == "gpt-5.6-sol"
        ));
    }

    #[test]
    fn provider_aware_selection_uses_readiness_priority_and_api_ids() {
        let catalog = portable_model_catalog();
        let openai = ProviderId::openai();
        let openrouter = ProviderId::new("openrouter");

        let offerings = catalog
            .list(None)
            .into_iter()
            .filter(|model| model.id.as_str() == "gpt-5.6-sol")
            .collect::<Vec<_>>();
        assert_eq!(offerings.len(), 2);

        let direct = catalog
            .select("gpt-56-sol", None, &HashSet::from([openai.clone()]))
            .unwrap();
        assert_eq!(direct.provider, openai);
        assert_eq!(direct.id, "gpt-5.6-sol");
        assert_eq!(catalog.settings_for(direct).unwrap().api_id, "gpt-5.6-sol");

        let aggregator = catalog
            .select("gpt-56-sol", None, &HashSet::from([openrouter.clone()]))
            .unwrap();
        assert_eq!(aggregator.provider, openrouter);
        assert_eq!(aggregator.id, "gpt-5.6-sol");
        assert_eq!(
            catalog.settings_for(aggregator).unwrap().api_id,
            "openai/gpt-5.6-sol"
        );

        let both = HashSet::from([ProviderId::openai(), ProviderId::new("openrouter")]);
        assert_eq!(
            catalog.select("portable", None, &both).unwrap().provider,
            ProviderId::openai()
        );
        assert_eq!(
            catalog
                .select("portable", Some(&ProviderId::new("openrouter")), &both,)
                .unwrap()
                .provider,
            ProviderId::new("openrouter")
        );
        assert!(matches!(
            catalog.select(
                "portable",
                Some(&ProviderId::new("openrouter")),
                &HashSet::from([ProviderId::openai()]),
            ),
            Err(ModelSelectionError::ProviderUnavailable { provider })
                if provider == ProviderId::new("openrouter")
        ));
    }

    #[test]
    fn selection_fallback_preserves_ready_preference_per_request() {
        let catalog = portable_model_catalog();
        let openai = ProviderId::openai();
        let openrouter = ProviderId::new("openrouter");
        let ready = HashSet::from([openrouter.clone()]);

        let shared = catalog
            .resolve_selection_with_catalog_fallback(Some("portable"), None, &ready)
            .unwrap();
        assert_eq!(shared.provider, openrouter);

        let pinned = catalog
            .resolve_selection_with_catalog_fallback(Some("portable"), Some(&openai), &ready)
            .unwrap();
        assert_eq!(pinned.provider, openai);

        let unknown = catalog
            .resolve_selection_with_catalog_fallback(Some("provider-private-preview"), None, &ready)
            .unwrap();
        assert_eq!(unknown.provider, ProviderId::new("openrouter"));
        assert_eq!(unknown.model, "provider-private-preview");
    }

    #[test]
    fn legacy_builtin_selector_uses_readiness_priority_and_explicit_pins() {
        let catalog = portable_model_catalog();
        let openai = ProviderId::openai();
        let openrouter = ProviderId::new("openrouter");
        let selector = "openai/gpt-5.6-sol";

        for (eligible, expected_provider) in [
            (HashSet::from([openai.clone()]), openai.clone()),
            (HashSet::from([openrouter.clone()]), openrouter.clone()),
            (
                HashSet::from([openai.clone(), openrouter.clone()]),
                openai.clone(),
            ),
        ] {
            let selected = catalog
                .resolve_selection(Some(selector), None, &eligible)
                .unwrap();
            assert_eq!(selected.provider, expected_provider);
            assert_eq!(selected.model, "gpt-5.6-sol");
        }

        let both = HashSet::from([openai, openrouter.clone()]);
        let selected = catalog
            .resolve_selection(Some(selector), Some(&openrouter), &both)
            .unwrap();
        assert_eq!(selected.provider, openrouter);
        assert_eq!(selected.model, "gpt-5.6-sol");
    }

    #[test]
    fn equal_provider_priorities_use_canonical_provider_id_as_tie_breaker() {
        let catalog = Catalog::from_settings(&minimal_settings(
            r#"
[providers.zeta]
display_name = "Zeta"
adapter = "openai"
agent_profile = "openai"
priority = 10

[providers.zeta.models.zeta]
display_name = "Zeta"
family = "test"
aliases = ["shared"]
default = true

[providers.zeta.models.zeta.limits]
context_window = 1000

[providers.zeta.models.zeta.features]
tools = false
vision = false
reasoning = false

[providers.alpha]
display_name = "Alpha"
adapter = "openai"
agent_profile = "openai"
priority = 10

[providers.alpha.models.alpha]
display_name = "Alpha"
family = "test"
aliases = ["shared"]
default = true

[providers.alpha.models.alpha.limits]
context_window = 1000

[providers.alpha.models.alpha.features]
tools = false
vision = false
reasoning = false
"#,
        ))
        .unwrap();

        let eligible = HashSet::from([ProviderId::new("zeta"), ProviderId::new("alpha")]);
        assert_eq!(
            catalog.select("shared", None, &eligible).unwrap().provider,
            ProviderId::new("alpha")
        );
    }

    #[test]
    fn canonical_id_wins_over_cross_provider_alias() {
        let catalog = Catalog::from_settings(&minimal_settings(
            r#"
[providers.direct]
display_name = "Direct"
adapter = "openai"
agent_profile = "openai"
priority = 1

[providers.direct.models.shared]
display_name = "Canonical Shared"
family = "test"
default = true

[providers.direct.models.shared.limits]
context_window = 1000

[providers.direct.models.shared.features]
tools = false
vision = false
reasoning = false

[providers.aggregator]
display_name = "Aggregator"
adapter = "openai"
agent_profile = "openai"
priority = 100

[providers.aggregator.models.other]
display_name = "Alias Shared"
family = "test"
aliases = ["shared"]
default = true

[providers.aggregator.models.other.limits]
context_window = 1000

[providers.aggregator.models.other.features]
tools = false
vision = false
reasoning = false
"#,
        ))
        .unwrap();
        let eligible = HashSet::from([ProviderId::new("direct"), ProviderId::new("aggregator")]);

        let unqualified = catalog.select("shared", None, &eligible).unwrap();
        assert_eq!(unqualified.provider, ProviderId::new("direct"));
        assert_eq!(unqualified.id, "shared");

        let qualified = catalog
            .resolve_on_provider(&ProviderId::new("aggregator"), "shared")
            .unwrap();
        assert_eq!(qualified.id, "other");

        let aggregator_only = HashSet::from([ProviderId::new("aggregator")]);
        let portable_alias = catalog.select("shared", None, &aggregator_only).unwrap();
        assert_eq!(portable_alias.provider, ProviderId::new("aggregator"));
        assert_eq!(portable_alias.id, "other");
    }

    #[test]
    fn empty_api_id_is_rejected() {
        let error = Catalog::from_settings(&minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[providers.test.models.model]
api_id = ""
display_name = "Model"
family = "test"
default = true

[providers.test.models.model.limits]
context_window = 1000

[providers.test.models.model.features]
tools = false
vision = false
reasoning = false
"#,
        ))
        .unwrap_err();

        assert!(matches!(
            error,
            CatalogBuildError::EmptyModelApiId { provider, model }
                if provider == ProviderId::new("test") && model == "model"
        ));
    }

    #[test]
    fn catalog_from_settings_filters_disabled_providers_and_models() {
        let layer = minimal_settings(
            r#"
[providers.enabled]
display_name = "Enabled"
adapter = "openai"
agent_profile = "openai"
enabled = true

[providers.disabled]
enabled = false

[models.enabled_model]
provider = "enabled"
display_name = "Enabled Model"
family = "test"
aliases = ["enabled-alias"]
default = true

[models.enabled_model.limits]
context_window = 1000

[models.enabled_model.features]
tools = false
vision = false
reasoning = false

[models.disabled_model]
provider = "enabled"
display_name = "Disabled Model"
family = "test"
aliases = ["disabled-alias"]
enabled = false

[models.disabled_model.limits]
context_window = 1000

[models.disabled_model.features]
tools = false
vision = false
reasoning = false

[models.model_on_disabled_provider]
provider = "disabled"
display_name = "Hidden"
family = "test"

[models.model_on_disabled_provider.limits]
context_window = 1000

[models.model_on_disabled_provider.features]
tools = false
vision = false
reasoning = false
"#,
        );

        let catalog = Catalog::from_settings(&layer).unwrap();

        assert!(catalog.get("enabled_model").is_some());
        assert!(catalog.get("enabled-alias").is_some());
        assert!(catalog.get("disabled_model").is_none());
        assert!(catalog.get("disabled-alias").is_none());
        assert!(catalog.get("model_on_disabled_provider").is_none());
        assert!(catalog.provider(&ProviderId::new("disabled")).is_none());
    }

    #[test]
    fn provider_priority_drives_configured_default_ordering() {
        let layer = minimal_settings(
            r#"
[providers.low]
display_name = "Low"
adapter = "openai"
agent_profile = "openai"
priority = 10

[providers.high]
display_name = "High"
adapter = "openai"
agent_profile = "openai"
priority = 20

[models.low_default]
provider = "low"
display_name = "Low Default"
family = "test"
default = true

[models.low_default.limits]
context_window = 1000

[models.low_default.features]
tools = false
vision = false
reasoning = false

[models.high_default]
provider = "high"
display_name = "High Default"
family = "test"
default = true

[models.high_default.limits]
context_window = 1000

[models.high_default.features]
tools = false
vision = false
reasoning = false
"#,
        );
        let catalog = Catalog::from_settings(&layer).unwrap();

        assert_eq!(catalog.default_model().id, "high_default");
        assert_eq!(
            catalog
                .default_for_configured_ids(&[ProviderId::new("low"), ProviderId::new("high")])
                .id,
            "high_default"
        );
        assert_eq!(
            catalog
                .default_for_configured_ids(&[ProviderId::new("low")])
                .id,
            "low_default"
        );
    }

    #[test]
    fn catalog_lists_models_by_provider_priority_then_model_id() {
        let layer = minimal_settings(
            r#"
[providers.zeta]
display_name = "Zeta"
adapter = "openai"
agent_profile = "openai"
priority = 20

[providers.alpha]
display_name = "Alpha"
adapter = "openai"
agent_profile = "openai"
priority = 10

[models.zeta_two]
provider = "zeta"
display_name = "Zeta Two"
family = "test"
default = true

[models.zeta_two.limits]
context_window = 1000

[models.zeta_two.features]
tools = false
vision = false
reasoning = false

[models.alpha_one]
provider = "alpha"
display_name = "Alpha One"
family = "test"
default = true

[models.alpha_one.limits]
context_window = 1000

[models.alpha_one.features]
tools = false
vision = false
reasoning = false

[models.zeta_one]
provider = "zeta"
display_name = "Zeta One"
family = "test"

[models.zeta_one.limits]
context_window = 1000

[models.zeta_one.features]
tools = false
vision = false
reasoning = false
"#,
        );
        let catalog = Catalog::from_settings(&layer).unwrap();

        let ids = catalog
            .list(None)
            .into_iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, ["zeta_one", "zeta_two", "alpha_one"]);
        assert_eq!(catalog.default_model().id, "zeta_two");
    }

    #[test]
    fn provider_aliases_resolve_provider_scoped_catalog_methods() {
        let layer = minimal_settings(
            r#"
[providers.canonical]
display_name = "Canonical"
adapter = "openai"
agent_profile = "openai"
aliases = ["alias"]

[models.default_model]
provider = "canonical"
display_name = "Default Model"
family = "test"
default = true

[models.default_model.limits]
context_window = 1000

[models.default_model.features]
tools = false
vision = false
reasoning = false
"#,
        );
        let catalog = Catalog::from_settings(&layer).unwrap();
        let alias = ProviderId::new("alias");
        let reference = catalog.get("default_model").unwrap();

        assert_eq!(
            catalog.provider(&alias).unwrap().id,
            ProviderId::new("canonical")
        );
        assert_eq!(
            catalog.default_for_provider(&alias).unwrap().id,
            "default_model"
        );
        assert_eq!(
            catalog
                .default_for_configured_ids(std::slice::from_ref(&alias))
                .id,
            "default_model"
        );
        assert_eq!(catalog.list(Some(&alias))[0].id, "default_model");
        assert_eq!(
            catalog.closest(&alias, reference).unwrap().id,
            "default_model"
        );
    }

    #[test]
    fn probe_for_provider_prefers_enabled_probe_model_over_provider_default() {
        let layer = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.default_model]
provider = "test"
display_name = "Default Model"
family = "test"
default = true

[models.default_model.limits]
context_window = 1000

[models.default_model.features]
tools = false
vision = false
reasoning = false

[models.probe_model]
provider = "test"
display_name = "Probe Model"
family = "test"
probe = true

[models.probe_model.limits]
context_window = 1000

[models.probe_model.features]
tools = false
vision = false
reasoning = false
"#,
        );
        let catalog = Catalog::from_settings(&layer).unwrap();

        assert_eq!(
            catalog
                .probe_for_provider(&ProviderId::new("test"))
                .unwrap()
                .id,
            "probe_model"
        );
    }

    #[test]
    fn probe_for_provider_falls_back_to_provider_default_when_no_probe_marked() {
        let layer = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.default_model]
provider = "test"
display_name = "Default Model"
family = "test"
default = true

[models.default_model.limits]
context_window = 1000

[models.default_model.features]
tools = false
vision = false
reasoning = false

[models.other_model]
provider = "test"
display_name = "Other Model"
family = "test"

[models.other_model.limits]
context_window = 1000

[models.other_model.features]
tools = false
vision = false
reasoning = false
"#,
        );
        let catalog = Catalog::from_settings(&layer).unwrap();

        assert_eq!(
            catalog
                .probe_for_provider(&ProviderId::new("test"))
                .unwrap()
                .id,
            "default_model"
        );
    }

    #[test]
    fn probe_false_override_clears_inherited_builtin_probe_marker() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r#"
[models."gpt-5.4-mini"]
probe = false
"#,
        ))
        .expect("sparse built-in model override should build");

        let openai = ProviderId::openai();
        assert_eq!(
            catalog.probe_for_provider(&openai).unwrap().id,
            catalog.default_for_provider(&openai).unwrap().id
        );
    }

    #[test]
    fn probe_for_provider_resolves_provider_alias() {
        let layer = minimal_settings(
            r#"
[providers.canonical]
display_name = "Canonical"
adapter = "openai"
agent_profile = "openai"
aliases = ["alias"]

[models.default_model]
provider = "canonical"
display_name = "Default Model"
family = "test"
default = true

[models.default_model.limits]
context_window = 1000

[models.default_model.features]
tools = false
vision = false
reasoning = false

[models.probe_model]
provider = "canonical"
display_name = "Probe Model"
family = "test"
probe = true

[models.probe_model.limits]
context_window = 1000

[models.probe_model.features]
tools = false
vision = false
reasoning = false
"#,
        );
        let catalog = Catalog::from_settings(&layer).unwrap();

        assert_eq!(
            catalog
                .probe_for_provider(&ProviderId::new("alias"))
                .unwrap()
                .id,
            "probe_model"
        );
    }

    #[test]
    fn small_default_for_provider_prefers_enabled_small_default_model_over_provider_default() {
        let layer = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.default_model]
provider = "test"
display_name = "Default Model"
family = "test"
default = true

[models.default_model.limits]
context_window = 1000

[models.default_model.features]
tools = false
vision = false
reasoning = false

[models.small_model]
provider = "test"
display_name = "Small Model"
family = "test"
small_default = true

[models.small_model.limits]
context_window = 1000

[models.small_model.features]
tools = false
vision = false
reasoning = false
"#,
        );
        let catalog = Catalog::from_settings(&layer).unwrap();

        assert_eq!(
            catalog
                .small_default_for_provider(&ProviderId::new("test"))
                .unwrap()
                .id,
            "small_model"
        );
    }

    #[test]
    fn small_default_for_provider_returns_none_when_no_small_default_marked() {
        let layer = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.default_model]
provider = "test"
display_name = "Default Model"
family = "test"
default = true

[models.default_model.limits]
context_window = 1000

[models.default_model.features]
tools = false
vision = false
reasoning = false

[models.other_model]
provider = "test"
display_name = "Other Model"
family = "test"

[models.other_model.limits]
context_window = 1000

[models.other_model.features]
tools = false
vision = false
reasoning = false
"#,
        );
        let catalog = Catalog::from_settings(&layer).unwrap();

        assert!(
            catalog
                .small_default_for_provider(&ProviderId::new("test"))
                .is_none()
        );
    }

    #[test]
    fn small_default_for_provider_resolves_provider_alias() {
        let layer = minimal_settings(
            r#"
[providers.canonical]
display_name = "Canonical"
adapter = "openai"
agent_profile = "openai"
aliases = ["alias"]

[models.default_model]
provider = "canonical"
display_name = "Default Model"
family = "test"
default = true

[models.default_model.limits]
context_window = 1000

[models.default_model.features]
tools = false
vision = false
reasoning = false

[models.small_model]
provider = "canonical"
display_name = "Small Model"
family = "test"
small_default = true

[models.small_model.limits]
context_window = 1000

[models.small_model.features]
tools = false
vision = false
reasoning = false
"#,
        );
        let catalog = Catalog::from_settings(&layer).unwrap();

        assert_eq!(
            catalog
                .small_default_for_provider(&ProviderId::new("alias"))
                .unwrap()
                .id,
            "small_model"
        );
    }

    #[test]
    fn small_default_for_configured_ids_uses_highest_priority_configured_provider() {
        let layer = minimal_settings(
            r#"
[providers.low]
display_name = "Low"
adapter = "openai"
agent_profile = "openai"
priority = 10

[providers.high]
display_name = "High"
adapter = "openai"
agent_profile = "openai"
priority = 20

[models.low_default]
provider = "low"
display_name = "Low Default"
family = "test"
default = true

[models.low_default.limits]
context_window = 1000

[models.low_default.features]
tools = false
vision = false
reasoning = false

[models.low_small]
provider = "low"
display_name = "Low Small"
family = "test"
small_default = true

[models.low_small.limits]
context_window = 1000

[models.low_small.features]
tools = false
vision = false
reasoning = false

[models.high_default]
provider = "high"
display_name = "High Default"
family = "test"
default = true

[models.high_default.limits]
context_window = 1000

[models.high_default.features]
tools = false
vision = false
reasoning = false

[models.high_small]
provider = "high"
display_name = "High Small"
family = "test"
small_default = true

[models.high_small.limits]
context_window = 1000

[models.high_small.features]
tools = false
vision = false
reasoning = false
"#,
        );
        let catalog = Catalog::from_settings(&layer).unwrap();

        assert_eq!(
            catalog
                .small_default_for_configured_ids(&[
                    ProviderId::new("low"),
                    ProviderId::new("high")
                ])
                .id,
            "high_small"
        );
        assert_eq!(
            catalog
                .small_default_for_configured_ids(&[ProviderId::new("low")])
                .id,
            "low_small"
        );
        assert_eq!(
            catalog.small_default_for_configured_ids(&[]).id,
            catalog.default_model().id
        );
    }

    #[test]
    fn small_default_for_configured_ids_skips_provider_without_a_small_model() {
        let layer = minimal_settings(
            r#"
[providers.low]
display_name = "Low"
adapter = "openai"
agent_profile = "openai"
priority = 10

[providers.high]
display_name = "High"
adapter = "openai"
agent_profile = "openai"
priority = 20

[models.low_small]
provider = "low"
display_name = "Low Small"
family = "test"
small_default = true

[models.low_small.limits]
context_window = 1000

[models.low_small.features]
tools = false
vision = false
reasoning = false

[models.high_default]
provider = "high"
display_name = "High Default"
family = "test"
default = true

[models.high_default.limits]
context_window = 1000

[models.high_default.features]
tools = false
vision = false
reasoning = false
"#,
        );
        let catalog = Catalog::from_settings(&layer).unwrap();

        // `high` outranks `low` but marks no small default, so selection moves
        // on rather than substituting `high_default`.
        assert_eq!(
            catalog
                .small_default_for_configured_ids(&[
                    ProviderId::new("low"),
                    ProviderId::new("high")
                ])
                .id,
            "low_small"
        );
    }

    #[test]
    fn small_default_for_configured_ids_falls_back_to_provider_default() {
        let layer = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.default_model]
provider = "test"
display_name = "Default Model"
family = "test"
default = true

[models.default_model.limits]
context_window = 1000

[models.default_model.features]
tools = false
vision = false
reasoning = false
"#,
        );
        let catalog = Catalog::from_settings(&layer).unwrap();

        assert_eq!(
            catalog
                .small_default_for_configured_ids(&[ProviderId::new("test")])
                .id,
            "default_model"
        );
    }

    #[test]
    fn multiple_small_default_models_for_provider_fail_catalog_build() {
        let layer = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.default_model]
provider = "test"
display_name = "Default Model"
family = "test"
default = true

[models.default_model.limits]
context_window = 1000

[models.default_model.features]
tools = false
vision = false
reasoning = false

[models.first_small]
provider = "test"
display_name = "First Small"
family = "test"
small_default = true

[models.first_small.limits]
context_window = 1000

[models.first_small.features]
tools = false
vision = false
reasoning = false

[models.second_small]
provider = "test"
display_name = "Second Small"
family = "test"
small_default = true

[models.second_small.limits]
context_window = 1000

[models.second_small.features]
tools = false
vision = false
reasoning = false
"#,
        );

        let err = Catalog::from_settings(&layer).unwrap_err();

        assert!(matches!(
            err,
            CatalogBuildError::MultipleProviderSmallDefaults { provider, models }
                if provider == ProviderId::new("test")
                    && models == vec!["first_small".to_string(), "second_small".to_string()]
        ));
    }

    #[test]
    fn small_default_false_override_clears_inherited_builtin_small_default_marker() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r#"
[models."gpt-5.4-mini"]
small_default = false
"#,
        ))
        .expect("sparse built-in model override should build");

        let openai = ProviderId::openai();
        assert!(catalog.small_default_for_provider(&openai).is_none());
    }

    #[test]
    fn multiple_probe_models_are_non_fatal_and_select_a_probe_model() {
        let layer = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.default_model]
provider = "test"
display_name = "Default Model"
family = "test"
default = true

[models.default_model.limits]
context_window = 1000

[models.default_model.features]
tools = false
vision = false
reasoning = false

[models.first_probe]
provider = "test"
display_name = "First Probe"
family = "test"
probe = true

[models.first_probe.limits]
context_window = 1000

[models.first_probe.features]
tools = false
vision = false
reasoning = false

[models.second_probe]
provider = "test"
display_name = "Second Probe"
family = "test"
probe = true

[models.second_probe.limits]
context_window = 1000

[models.second_probe.features]
tools = false
vision = false
reasoning = false
"#,
        );
        let catalog = Catalog::from_settings(&layer).unwrap();
        let selected = catalog
            .probe_for_provider(&ProviderId::new("test"))
            .unwrap()
            .id
            .as_str();

        assert!(["first_probe", "second_probe"].contains(&selected));
        assert_ne!(selected, "default_model");
    }

    #[test]
    fn provider_agent_profile_overrides_adapter_default() {
        let layer = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai_compatible"
base_url = "https://api.test/v1"
agent_profile = "anthropic"

[models.default_model]
provider = "test"
display_name = "Default Model"
family = "test"
default = true

[models.default_model.limits]
context_window = 1000

[models.default_model.features]
tools = false
vision = false
reasoning = false
"#,
        );

        let catalog = Catalog::from_settings(&layer).unwrap();

        assert_eq!(
            catalog
                .provider(&ProviderId::new("test"))
                .unwrap()
                .agent_profile,
            AgentProfileKind::Anthropic
        );
        assert_eq!(
            catalog.effective_agent_profile(&ProviderId::new("test"), Some("default_model")),
            Some(AgentProfileKind::Anthropic)
        );
    }

    #[test]
    fn adapter_defaults_provider_agent_profile_and_billing_policy() {
        let settings = minimal_settings(
            r#"
[providers.anthropic]
display_name = "Anthropic"
adapter = "anthropic"

[providers.openai]
display_name = "OpenAI"
adapter = "openai"

[providers.gemini]
display_name = "Gemini"
adapter = "gemini"

[providers.compat]
display_name = "Compatible"
adapter = "openai_compatible"
"#,
        );

        let providers = build_providers(&settings).unwrap();
        let provider = |id: &str| {
            providers
                .iter()
                .find(|provider| provider.id.as_str() == id)
                .unwrap()
        };

        assert_eq!(
            provider("anthropic").agent_profile,
            AgentProfileKind::Anthropic
        );
        assert_eq!(
            provider("anthropic").billing_policy,
            BillingPolicy::Anthropic
        );
        assert_eq!(provider("openai").agent_profile, AgentProfileKind::OpenAi);
        assert_eq!(provider("openai").billing_policy, BillingPolicy::OpenAi);
        assert_eq!(provider("gemini").agent_profile, AgentProfileKind::Gemini);
        assert_eq!(provider("gemini").billing_policy, BillingPolicy::Gemini);
        assert_eq!(provider("compat").agent_profile, AgentProfileKind::OpenAi);
        assert_eq!(provider("compat").billing_policy, BillingPolicy::OpenAi);
    }

    #[test]
    fn model_agent_profile_overrides_provider_profile_for_same_provider() {
        let layer = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "anthropic"
aliases = ["alias"]

[models.default_model]
provider = "test"
display_name = "Default Model"
family = "test"
default = true
agent_profile = "gemini"
aliases = ["default-alias"]

[models.default_model.limits]
context_window = 1000

[models.default_model.features]
tools = false
vision = false
reasoning = false
"#,
        );

        let catalog = Catalog::from_settings(&layer).unwrap();

        assert_eq!(
            catalog
                .model_settings("default-alias")
                .unwrap()
                .agent_profile,
            AgentProfileKind::Gemini
        );
        assert_eq!(
            catalog.effective_agent_profile(&ProviderId::new("alias"), Some("default-alias")),
            Some(AgentProfileKind::Gemini)
        );
    }

    #[test]
    fn effective_agent_profile_does_not_leak_unrelated_model_override() {
        let layer = minimal_settings(
            r#"
[providers.one]
display_name = "One"
adapter = "openai"
agent_profile = "openai"

[providers.two]
display_name = "Two"
adapter = "openai"
agent_profile = "anthropic"

[models.one_model]
provider = "one"
display_name = "One Model"
family = "test"
default = true

[models.one_model.limits]
context_window = 1000

[models.one_model.features]
tools = false
vision = false
reasoning = false

[models.two_model]
provider = "two"
display_name = "Two Model"
family = "test"
default = true
agent_profile = "gemini"

[models.two_model.limits]
context_window = 1000

[models.two_model.features]
tools = false
vision = false
reasoning = false
"#,
        );

        let catalog = Catalog::from_settings(&layer).unwrap();

        assert_eq!(
            catalog.effective_agent_profile(&ProviderId::new("one"), Some("two_model")),
            Some(AgentProfileKind::OpenAi)
        );
    }

    #[test]
    fn effective_agent_profile_is_scoped_by_provider_for_shared_model_id() {
        let layer = minimal_settings(
            r#"
[providers.one]
display_name = "One"
adapter = "openai"
agent_profile = "openai"

[providers.one.models.shared]
display_name = "Shared on One"
family = "test"
default = true

[providers.one.models.shared.limits]
context_window = 1000

[providers.one.models.shared.features]
tools = false
vision = false
reasoning = false

[providers.two]
display_name = "Two"
adapter = "openai"
agent_profile = "anthropic"

[providers.two.models.shared]
display_name = "Shared on Two"
family = "test"
default = true
agent_profile = "gemini"

[providers.two.models.shared.limits]
context_window = 1000

[providers.two.models.shared.features]
tools = false
vision = false
reasoning = false
"#,
        );

        let catalog = Catalog::from_settings(&layer).unwrap();

        assert_eq!(
            catalog.effective_agent_profile(&ProviderId::new("one"), Some("shared")),
            Some(AgentProfileKind::OpenAi)
        );
        assert_eq!(
            catalog.effective_agent_profile(&ProviderId::new("two"), Some("shared")),
            Some(AgentProfileKind::Gemini)
        );
    }

    #[test]
    fn omitted_agent_profile_uses_adapter_default() {
        let layer = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "gemini"

[models.default_model]
provider = "test"
display_name = "Default Model"
family = "test"
default = true

[models.default_model.limits]
context_window = 1000

[models.default_model.features]
tools = false
vision = false
reasoning = false
"#,
        );

        let catalog = Catalog::from_settings(&layer).unwrap();

        assert_eq!(
            catalog
                .provider(&ProviderId::new("test"))
                .unwrap()
                .agent_profile,
            AgentProfileKind::Gemini
        );
        assert_eq!(
            catalog.effective_agent_profile(&ProviderId::new("test"), Some("default_model")),
            Some(AgentProfileKind::Gemini)
        );
    }

    #[test]
    fn provider_auth_modes_and_billing_policy_are_catalog_owned() {
        let settings = minimal_settings(
            r#"
[providers.bearer]
display_name = "Bearer"
adapter = "openai"

[providers.bearer.auth]
credentials = ["env:BEARER_API_KEY", "vault:BEARER_API_KEY"]

[providers.custom]
display_name = "Custom"
adapter = "gemini"

[providers.custom.auth]
credentials = ["env:CUSTOM_API_KEY"]
header = { custom = "x-api-key" }

[providers.none]
display_name = "No Auth"
adapter = "openai_compatible"
billing_policy = "none"
"#,
        );

        let providers = build_providers(&settings).unwrap();
        let provider = |id: &str| {
            providers
                .iter()
                .find(|provider| provider.id.as_str() == id)
                .unwrap()
        };

        let bearer = provider("bearer");
        assert_eq!(bearer.billing_policy, BillingPolicy::OpenAi);
        assert_eq!(
            bearer.auth,
            Some(ProviderAuthConfig {
                credentials: vec![
                    CredentialRef::Env("BEARER_API_KEY".to_string()),
                    CredentialRef::Vault("BEARER_API_KEY".to_string()),
                ],
                header:      ApiKeyHeaderPolicy::Bearer,
            })
        );

        let custom = provider("custom");
        assert_eq!(custom.billing_policy, BillingPolicy::Gemini);
        assert_eq!(
            custom.auth,
            Some(ProviderAuthConfig {
                credentials: vec![CredentialRef::Env("CUSTOM_API_KEY".to_string())],
                header:      ApiKeyHeaderPolicy::Custom {
                    name: "x-api-key".to_string(),
                },
            })
        );

        let no_auth = provider("none");
        assert_eq!(no_auth.billing_policy, BillingPolicy::None);
        assert!(no_auth.auth.is_none());
    }

    #[test]
    fn provider_auth_header_defaults_to_bearer_when_omitted() {
        let settings = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"

[providers.test.auth]
credentials = ["env:TEST_API_KEY"]
"#,
        );
        let providers = build_providers(&settings).unwrap();
        let test = providers
            .iter()
            .find(|provider| provider.id.as_str() == "test")
            .unwrap();
        assert_eq!(
            test.auth.as_ref().unwrap().header,
            ApiKeyHeaderPolicy::Bearer
        );
    }

    #[test]
    fn catalog_from_settings_rejects_invalid_provider_auth_configs() {
        let empty_api_key_credentials = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[providers.test.auth]
credentials = []
"#,
        );
        assert!(matches!(
            Catalog::from_settings(&empty_api_key_credentials).unwrap_err(),
            CatalogBuildError::EmptyApiKeyCredentials { provider }
                if provider == ProviderId::new("test")
        ));

        let sigv4_on_openai = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[providers.test.auth]
credentials = ["aws_sigv4"]
"#,
        );
        assert!(matches!(
            Catalog::from_settings(&sigv4_on_openai).unwrap_err(),
            CatalogBuildError::UnsupportedAwsSigv4Credential { provider, adapter }
                if provider == ProviderId::new("test") && adapter == AdapterKind::OpenAi
        ));
    }

    #[test]
    fn provider_auth_deserialization_rejects_invalid_auth_shape() {
        let invalid_header = toml::from_str::<LlmCatalogSettings>(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[providers.test.auth]
credentials = ["env:TEST_API_KEY"]
header = { custom = "bad header" }
"#,
        )
        .unwrap_err();
        assert!(
            invalid_header
                .to_string()
                .contains("custom header name must be a valid HTTP header name")
        );

        let legacy_type_tag = toml::from_str::<LlmCatalogSettings>(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[providers.test.auth]
type = "api_key"
credentials = ["env:TEST_API_KEY"]
"#,
        )
        .unwrap_err();
        assert!(
            legacy_type_tag.to_string().contains("unknown field `type`"),
            "expected unknown-field error for legacy `type` key, got: {legacy_type_tag}"
        );
    }

    #[test]
    fn catalog_from_settings_validates_model_controls_and_speed_costs() {
        let invalid_effort = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.model]
provider = "test"
display_name = "Model"
family = "test"
default = true

[models.model.limits]
context_window = 1000

[models.model.features]
tools = false
vision = false
reasoning = true
reasoning_effort = "levels"

[models.model.controls]
reasoning_effort = ["turbo"]
"#,
        );
        assert!(matches!(
            Catalog::from_settings(&invalid_effort).unwrap_err(),
            CatalogBuildError::InvalidReasoningEffort { model, value, .. }
                if model == "model" && value == "turbo"
        ));

        let undeclared_speed_cost = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "anthropic"
agent_profile = "anthropic"

[models.model]
provider = "test"
display_name = "Model"
family = "test"
default = true

[models.model.limits]
context_window = 1000

[models.model.features]
tools = false
vision = false
reasoning = false

[models.model.costs.speed.fast]
input_cost_per_mtok = 1.0
"#,
        );
        assert!(matches!(
            Catalog::from_settings(&undeclared_speed_cost).unwrap_err(),
            CatalogBuildError::UndeclaredSpeedCost { model, speed }
                if model == "model" && speed == Speed::Fast
        ));
    }

    #[test]
    fn catalog_from_settings_accepts_reasoning_effort_feature_levels() {
        let settings = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.model]
provider = "test"
display_name = "Model"
family = "test"
default = true

[models.model.limits]
context_window = 1000

[models.model.features]
tools = true
vision = false
reasoning = true
reasoning_by_default = false
reasoning_effort = "levels"
prompt_cache = true

[models.model.controls]
reasoning_effort = ["low", "medium"]
"#,
        );

        let catalog = Catalog::from_settings(&settings).unwrap();
        let model = catalog.get("model").unwrap();
        assert_eq!(
            model.features.reasoning_effort,
            crate::ReasoningEffortFeature::Levels
        );
        assert!(model.features.prompt_cache);
        assert!(
            !catalog
                .model_settings("model")
                .unwrap()
                .reasoning_by_default
        );
        assert_eq!(
            catalog
                .model_settings("model")
                .unwrap()
                .controls
                .reasoning_effort,
            vec![ReasoningEffort::Low, ReasoningEffort::Medium]
        );
    }

    #[test]
    fn catalog_from_settings_accepts_reasoning_effort_feature_always_adaptive() {
        let settings = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.model]
provider = "test"
display_name = "Model"
family = "test"
default = true

[models.model.limits]
context_window = 1000

[models.model.features]
tools = true
vision = false
reasoning = true
reasoning_effort = "always_adaptive"
prompt_cache = true
"#,
        );

        let catalog = Catalog::from_settings(&settings).unwrap();
        let model = catalog.get("model").unwrap();
        assert_eq!(
            model.features.reasoning_effort,
            crate::ReasoningEffortFeature::AlwaysAdaptive
        );
        assert!(model.supports_reasoning_effort());
        assert!(
            catalog
                .model_settings("model")
                .unwrap()
                .reasoning_by_default
        );
        // Always-adaptive models get the full default effort controls, same as
        // Levels.
        assert_eq!(
            catalog
                .model_settings("model")
                .unwrap()
                .controls
                .reasoning_effort,
            ReasoningEffort::VARIANTS.to_vec()
        );
    }

    #[test]
    fn catalog_from_settings_accepts_reasoning_effort_controls_without_native_effort_feature() {
        let settings = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.model]
provider = "test"
display_name = "Model"
family = "test"
default = true

[models.model.limits]
context_window = 1000

[models.model.features]
tools = true
vision = false
reasoning = true
reasoning_by_default = true
reasoning_effort = "none"

[models.model.controls]
reasoning_effort = ["low"]
"#,
        );

        let catalog = Catalog::from_settings(&settings).unwrap();
        let model = catalog.get("model").unwrap();
        assert_eq!(
            model.features.reasoning_effort,
            crate::ReasoningEffortFeature::None
        );
        assert!(
            catalog
                .model_settings("model")
                .unwrap()
                .reasoning_by_default
        );
        assert_eq!(
            catalog
                .model_settings("model")
                .unwrap()
                .controls
                .reasoning_effort,
            vec![ReasoningEffort::Low]
        );
    }

    #[test]
    fn catalog_from_settings_rejects_reasoning_effort_controls_without_reasoning() {
        let settings = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.model]
provider = "test"
display_name = "Model"
family = "test"

[models.model.limits]
context_window = 1000

[models.model.features]
tools = true
vision = false
reasoning = false
reasoning_effort = "none"

[models.model.controls]
reasoning_effort = ["low"]
"#,
        );

        assert!(matches!(
            Catalog::from_settings(&settings).unwrap_err(),
            CatalogBuildError::ReasoningEffortControlsWithoutReasoning { model }
                if model == "model"
        ));
    }

    #[test]
    fn catalog_from_settings_rejects_reasoning_effort_feature_without_reasoning() {
        let settings = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.model]
provider = "test"
display_name = "Model"
family = "test"

[models.model.limits]
context_window = 1000

[models.model.features]
tools = true
vision = false
reasoning = false
reasoning_effort = "levels"
"#,
        );

        assert!(matches!(
            Catalog::from_settings(&settings).unwrap_err(),
            CatalogBuildError::ReasoningEffortWithoutReasoning { model }
                if model == "model"
        ));
    }

    #[test]
    fn catalog_from_settings_rejects_default_reasoning_without_reasoning() {
        let settings = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.model]
provider = "test"
display_name = "Model"
family = "test"

[models.model.limits]
context_window = 1000

[models.model.features]
tools = true
vision = false
reasoning = false
reasoning_by_default = true
"#,
        );

        assert!(matches!(
            Catalog::from_settings(&settings).unwrap_err(),
            CatalogBuildError::DefaultReasoningWithoutReasoning { model }
                if model == "model"
        ));
    }

    #[test]
    fn catalog_from_settings_rejects_cache_control_breakpoints_without_prompt_cache() {
        let settings = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "https://example.test/v1"

[models.model]
provider = "test"
display_name = "Model"
family = "test"

[models.model.limits]
context_window = 1000

[models.model.features]
tools = true
vision = false
reasoning = false
cache_control_breakpoints = true
"#,
        );

        assert!(matches!(
            Catalog::from_settings(&settings).unwrap_err(),
            CatalogBuildError::CacheControlBreakpointsWithoutPromptCache { model }
                if model == "model"
        ));
    }

    #[test]
    fn catalog_from_settings_rejects_always_adaptive_effort_without_reasoning() {
        let settings = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.model]
provider = "test"
display_name = "Model"
family = "test"
default = true

[models.model.limits]
context_window = 1000

[models.model.features]
tools = true
vision = false
reasoning = false
reasoning_effort = "always_adaptive"
"#,
        );

        assert!(matches!(
            Catalog::from_settings(&settings).unwrap_err(),
            CatalogBuildError::ReasoningEffortWithoutReasoning { model }
                if model == "model"
        ));
    }

    #[test]
    fn catalog_from_settings_sampling_params_defaults_true_and_accepts_false() {
        let settings = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.with-sampling]
provider = "test"
display_name = "With"
family = "test"
default = true

[models.with-sampling.limits]
context_window = 1000

[models.with-sampling.features]
tools = true
vision = false
reasoning = false

[models.no-sampling]
provider = "test"
display_name = "Without"
family = "test"

[models.no-sampling.limits]
context_window = 1000

[models.no-sampling.features]
tools = true
vision = false
reasoning = false
sampling_params = false
"#,
        );

        let catalog = Catalog::from_settings(&settings).unwrap();
        assert!(
            catalog
                .get("with-sampling")
                .unwrap()
                .features
                .sampling_params
        );
        assert!(!catalog.get("no-sampling").unwrap().features.sampling_params);
    }

    // ---- Provider / catalog data integrity tests ----

    #[test]
    fn every_provider_has_catalog_models() {
        let catalog = Catalog::builtin();
        for provider in catalog.providers() {
            let models = catalog.list(Some(&provider.id));
            assert!(
                !models.is_empty(),
                "Provider {:?} has no models in catalog",
                provider.id,
            );
        }
    }

    #[test]
    fn every_provider_has_exactly_one_default_model() {
        let catalog = Catalog::builtin();
        for provider in catalog.providers() {
            let defaults: Vec<_> = catalog
                .list(Some(&provider.id))
                .into_iter()
                .filter(|m| m.default)
                .collect();
            assert_eq!(
                defaults.len(),
                1,
                "Provider {:?} should have exactly one default model, found {}: {:?}",
                provider.id,
                defaults.len(),
                defaults.iter().map(|m| &m.id).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn every_catalog_model_provider_has_catalog_provider() {
        let catalog = Catalog::builtin();
        for model in catalog.list(None) {
            assert!(
                catalog.provider(&model.provider).is_some(),
                "catalog model '{}' provider {:?} has no provider metadata",
                model.id,
                model.provider,
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
            provider: anthropic,
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
                reasoning_effort: Levels,
                prompt_cache: true,
                cache_control_breakpoints: false,
                sampling_params: true,
            },
            controls: ModelControls {
                reasoning_effort: [
                    Low,
                    Medium,
                    High,
                    XHigh,
                    Max,
                ],
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(
                    5.0,
                ),
                output_cost_per_mtok: Some(
                    25.0,
                ),
                cache_input_cost_per_mtok: Some(
                    0.5,
                ),
            },
            estimated_output_tps: Some(
                25.0,
            ),
            aliases: [],
            default: false,
            small_default: false,
            configured: false,
        }
        "#);
    }

    #[test]
    fn get_model_info_returns_none_for_unknown() {
        assert!(Catalog::builtin().get("nonexistent-model").is_none());
    }

    #[test]
    fn kimi_k2_5_in_catalog() {
        let m = Catalog::builtin().get("kimi-k2.5").unwrap();
        insta::assert_debug_snapshot!(m, @r#"
        Model {
            id: "kimi-k2.5",
            provider: moonshot,
            family: "kimi-k2",
            display_name: "Kimi K2.5",
            limits: ModelLimits {
                context_window: 262144,
                max_output: Some(
                    32768,
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
                reasoning: true,
                reasoning_effort: None,
                prompt_cache: true,
                cache_control_breakpoints: false,
                sampling_params: false,
            },
            controls: ModelControls {
                reasoning_effort: [],
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(
                    0.6,
                ),
                output_cost_per_mtok: Some(
                    3.0,
                ),
                cache_input_cost_per_mtok: Some(
                    0.1,
                ),
            },
            estimated_output_tps: Some(
                50.0,
            ),
            aliases: [],
            default: false,
            small_default: false,
            configured: false,
        }
        "#);
    }

    #[test]
    fn kimi_k3_in_catalog() {
        let catalog = Catalog::builtin();
        let m = catalog.get("kimi-k3").unwrap();
        insta::assert_debug_snapshot!(m, @r#"
        Model {
            id: "kimi-k3",
            provider: moonshot,
            family: "kimi-k3",
            display_name: "Kimi K3",
            limits: ModelLimits {
                context_window: 1048576,
                max_output: Some(
                    131072,
                ),
            },
            training: None,
            knowledge_cutoff: None,
            features: ModelFeatures {
                tools: true,
                vision: true,
                reasoning: true,
                reasoning_effort: AlwaysAdaptive,
                prompt_cache: true,
                cache_control_breakpoints: false,
                sampling_params: false,
            },
            controls: ModelControls {
                reasoning_effort: [
                    Low,
                    High,
                    Max,
                ],
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(
                    3.0,
                ),
                output_cost_per_mtok: Some(
                    15.0,
                ),
                cache_input_cost_per_mtok: Some(
                    0.3,
                ),
            },
            estimated_output_tps: None,
            aliases: [
                "kimi",
            ],
            default: true,
            small_default: false,
            configured: false,
        }
        "#);
        assert_eq!(
            catalog
                .model_settings("kimi-k3")
                .unwrap()
                .controls
                .reasoning_effort,
            vec![
                ReasoningEffort::Low,
                ReasoningEffort::High,
                ReasoningEffort::Max,
            ]
        );
    }

    #[test]
    fn kimi_alias() {
        assert_eq!(Catalog::builtin().get("kimi").unwrap().id, "kimi-k3");
    }

    #[test]
    fn glm_4_7_in_catalog() {
        let m = Catalog::builtin().get("glm-4.7").unwrap();
        assert_eq!(m.provider, ProviderId::new("zai"));
        assert_eq!(Catalog::builtin().get("glm4").unwrap().id, "glm-4.7");
    }

    #[test]
    fn glm_5_2_in_catalog() {
        let catalog = Catalog::builtin();
        let model = catalog.get("glm-5.2").expect("GLM 5.2 should be present");
        insta::assert_debug_snapshot!(model, @r#"
        Model {
            id: "glm-5.2",
            provider: zai,
            family: "glm-5",
            display_name: "GLM 5.2",
            limits: ModelLimits {
                context_window: 1048576,
                max_output: Some(
                    131072,
                ),
            },
            training: None,
            knowledge_cutoff: None,
            features: ModelFeatures {
                tools: true,
                vision: false,
                reasoning: true,
                reasoning_effort: Levels,
                prompt_cache: true,
                cache_control_breakpoints: false,
                sampling_params: true,
            },
            controls: ModelControls {
                reasoning_effort: [
                    High,
                    Max,
                ],
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(
                    1.4,
                ),
                output_cost_per_mtok: Some(
                    4.4,
                ),
                cache_input_cost_per_mtok: Some(
                    0.26,
                ),
            },
            estimated_output_tps: None,
            aliases: [
                "glm52",
                "glm5.2",
            ],
            default: false,
            small_default: false,
            configured: false,
        }
        "#);

        let settings = catalog
            .model_settings("glm-5.2")
            .expect("GLM 5.2 settings should be present");
        assert_eq!(settings.api_id, "glm-5.2");
        assert_eq!(settings.controls.reasoning_effort, vec![
            ReasoningEffort::High,
            ReasoningEffort::Max
        ]);
        assert_eq!(catalog.get("glm52").unwrap().id, "glm-5.2");
        assert_eq!(catalog.get("glm5.2").unwrap().id, "glm-5.2");
    }

    #[test]
    fn glm_5_3_in_catalog() {
        let catalog = Catalog::builtin();
        let model = catalog.get("glm-5.3").expect("GLM 5.3 should be present");
        insta::assert_debug_snapshot!(model, @r#"
        Model {
            id: "glm-5.3",
            provider: zai,
            family: "glm-5",
            display_name: "GLM 5.3",
            limits: ModelLimits {
                context_window: 1048576,
                max_output: Some(
                    131072,
                ),
            },
            training: None,
            knowledge_cutoff: None,
            features: ModelFeatures {
                tools: true,
                vision: false,
                reasoning: true,
                reasoning_effort: Levels,
                prompt_cache: true,
                cache_control_breakpoints: false,
                sampling_params: true,
            },
            controls: ModelControls {
                reasoning_effort: [
                    Low,
                    High,
                    Max,
                ],
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(
                    1.4,
                ),
                output_cost_per_mtok: Some(
                    4.4,
                ),
                cache_input_cost_per_mtok: Some(
                    0.26,
                ),
            },
            estimated_output_tps: None,
            aliases: [
                "glm",
                "glm5",
                "glm53",
                "glm5.3",
            ],
            default: true,
            small_default: false,
            configured: false,
        }
        "#);

        let settings = catalog
            .model_settings("glm-5.3")
            .expect("GLM 5.3 settings should be present");
        assert_eq!(settings.api_id, "glm-5.3");
        assert_eq!(settings.controls.reasoning_effort, vec![
            ReasoningEffort::Low,
            ReasoningEffort::High,
            ReasoningEffort::Max
        ]);
        assert_eq!(catalog.get("glm").unwrap().id, "glm-5.3");
        assert_eq!(catalog.get("glm5").unwrap().id, "glm-5.3");
        assert_eq!(catalog.get("glm53").unwrap().id, "glm-5.3");
        assert_eq!(catalog.get("glm5.3").unwrap().id, "glm-5.3");
    }

    #[test]
    fn glm_5_3_flash_in_catalog() {
        let catalog = Catalog::builtin();
        let model = catalog
            .get("glm-5.3-flash")
            .expect("GLM 5.3 Flash should be present");
        insta::assert_debug_snapshot!(model, @r#"
        Model {
            id: "glm-5.3-flash",
            provider: zai,
            family: "glm-5",
            display_name: "GLM 5.3 Flash",
            limits: ModelLimits {
                context_window: 1048576,
                max_output: Some(
                    131072,
                ),
            },
            training: None,
            knowledge_cutoff: None,
            features: ModelFeatures {
                tools: true,
                vision: true,
                reasoning: true,
                reasoning_effort: Levels,
                prompt_cache: true,
                cache_control_breakpoints: false,
                sampling_params: true,
            },
            controls: ModelControls {
                reasoning_effort: [
                    Low,
                    High,
                    Max,
                ],
            },
            costs: ModelCosts {
                input_cost_per_mtok: Some(
                    0.15,
                ),
                output_cost_per_mtok: Some(
                    0.5,
                ),
                cache_input_cost_per_mtok: Some(
                    0.03,
                ),
            },
            estimated_output_tps: None,
            aliases: [],
            default: false,
            small_default: true,
            configured: false,
        }
        "#);

        let settings = catalog
            .model_settings("glm-5.3-flash")
            .expect("GLM 5.3 Flash settings should be present");
        assert_eq!(settings.api_id, "glm-5.3-flash");
        assert_eq!(
            catalog
                .small_default_for_provider(&ProviderId::new("zai"))
                .unwrap()
                .id,
            "glm-5.3-flash"
        );
    }

    #[test]
    fn minimax_m2_5_in_catalog() {
        let m = Catalog::builtin().get("minimax-m2.5").unwrap();
        assert_eq!(m.provider, ProviderId::new("minimax"));
    }

    #[test]
    fn mercury_2_in_catalog() {
        let m = Catalog::builtin().get("mercury-2").unwrap();
        insta::assert_debug_snapshot!(m, @r#"
        Model {
            id: "mercury-2",
            provider: inception,
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
                reasoning_effort: Levels,
                prompt_cache: false,
                cache_control_breakpoints: false,
                sampling_params: true,
            },
            controls: ModelControls {
                reasoning_effort: [
                    Low,
                    Medium,
                    High,
                    XHigh,
                    Max,
                ],
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
            small_default: false,
            configured: false,
        }
        "#);
    }

    #[test]
    fn mercury_alias_resolves_to_mercury_2() {
        assert_eq!(Catalog::builtin().get("mercury").unwrap().id, "mercury-2");
    }

    #[test]
    fn gpt_5_4_pro_in_catalog() {
        let m = Catalog::builtin().get("gpt-5.4-pro").unwrap();
        insta::assert_debug_snapshot!(m, @r#"
        Model {
            id: "gpt-5.4-pro",
            provider: openai,
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
                reasoning_effort: Levels,
                prompt_cache: false,
                cache_control_breakpoints: false,
                sampling_params: true,
            },
            controls: ModelControls {
                reasoning_effort: [
                    Low,
                    Medium,
                    High,
                    XHigh,
                    Max,
                ],
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
            small_default: false,
            configured: false,
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
    fn openai_codex_default_context_windows_match_codex_catalog() {
        let catalog = Catalog::builtin();

        for model in [
            "gpt-5.2",
            "gpt-5.3-codex",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.5",
            "gpt-5.6-luna",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
        ] {
            assert_eq!(
                catalog.get(model).unwrap().context_window(),
                272_000,
                "{model} should use the Codex-safe default context window"
            );
        }
    }

    #[test]
    fn openai_context_window_can_be_overridden_for_direct_api_usage() {
        let catalog = Catalog::from_builtin_with_overrides(&minimal_settings(
            r#"
[providers.openai.models."gpt-5.5".limits]
context_window = 1050000
"#,
        ))
        .expect("sparse built-in model limit override should build");

        let model = catalog.get("gpt-5.5").unwrap();
        assert_eq!(model.context_window(), 1_050_000);
        assert_eq!(model.max_output(), Some(128_000));
    }

    // ---- Closest model tests ----

    #[test]
    fn closest_model_sonnet_to_gemini() {
        let sonnet = Catalog::builtin().get("claude-sonnet-4-5").unwrap();
        let result = Catalog::builtin()
            .closest(&ProviderId::gemini(), sonnet)
            .unwrap();
        assert_eq!(result.id, "gemini-3.1-pro-preview");
    }

    #[test]
    fn closest_model_haiku_to_moonshot() {
        let haiku = Catalog::builtin().get("claude-haiku-4-5").unwrap();
        assert!(
            Catalog::builtin()
                .closest(&ProviderId::new("moonshot"), haiku)
                .is_none()
        );
    }

    #[test]
    fn closest_model_no_capability_match() {
        let glm = Catalog::builtin().get("glm-4.7").unwrap();
        assert!(
            Catalog::builtin()
                .closest(&ProviderId::gemini(), glm)
                .is_none()
        );
    }

    // ---- Cost tests ----

    #[test]
    fn model_info_costs() {
        let claude = Catalog::builtin().get("claude-opus-4-6").unwrap();
        assert_eq!(claude.costs.input_cost_per_mtok, Some(5.0));
        assert_eq!(claude.costs.output_cost_per_mtok, Some(25.0));

        let sonnet = Catalog::builtin().get("claude-sonnet-4-5").unwrap();
        assert_eq!(sonnet.costs.input_cost_per_mtok, Some(3.0));
    }
}
