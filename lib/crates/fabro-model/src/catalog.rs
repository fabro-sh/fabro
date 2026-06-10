use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;
use std::sync::LazyLock;

use rust_embed::RustEmbed;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use strum::VariantArray;
use toml::de::Error as TomlDeError;
use tracing::warn;

use crate::Speed;
use crate::adapter::{AdapterKind, AgentProfileKind};
use crate::ids::ProviderId;
use crate::provider::Provider;
use crate::reasoning::ReasoningEffort;
use crate::types::{Model, ModelCosts, ModelFeatures, ModelLimits, ReasoningEffortFeature};

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
    #[serde(default)]
    pub extra_headers:  Option<HashMap<String, HeaderValueRef>>,
    #[serde(default)]
    pub priority:       Option<i32>,
    #[serde(default)]
    pub enabled:        Option<bool>,
    #[serde(default)]
    pub aliases:        Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCatalogSettings {
    #[serde(default)]
    pub provider:             Option<String>,
    #[serde(default)]
    pub api_id:               Option<String>,
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
    pub tools:            Option<bool>,
    #[serde(default)]
    pub vision:           Option<bool>,
    #[serde(default)]
    pub reasoning:        Option<bool>,
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffortFeature>,
    #[serde(default)]
    pub prompt_cache:     Option<bool>,
    #[serde(default)]
    pub sampling_params:  Option<bool>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum CredentialRef {
    Vault(String),
    Env(String),
}

impl std::fmt::Display for CredentialRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Vault(name) => write!(f, "vault:{name}"),
            Self::Env(name) => write!(f, "env:{name}"),
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
    #[error("credential reference must be `vault:<name>` or `env:<NAME>`")]
    Invalid,
    #[error("credential reference is missing a name after `vault:`")]
    EmptyVault,
    #[error("credential reference is missing a name after `env:`")]
    EmptyEnv,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderAuthConfig {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderValueRef {
    Literal(String),
    Env(String),
    Vault(String),
}

impl Serialize for HeaderValueRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeMap;

        let mut map = serializer.serialize_map(Some(1))?;
        match self {
            Self::Literal(value) => map.serialize_entry("literal", value)?,
            Self::Env(value) => map.serialize_entry("env", value)?,
            Self::Vault(value) => map.serialize_entry("vault", value)?,
        }
        map.end()
    }
}

impl std::fmt::Display for HeaderValueRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Literal(_) => f.write_str("literal:<redacted>"),
            Self::Env(name) => write!(f, "env:{name}"),
            Self::Vault(id) => write!(f, "vault:{id}"),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum HeaderValueRefInput {
    Table(HeaderValueRefSerde),
    BareString(String),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeaderValueRefSerde {
    #[serde(default)]
    literal: Option<String>,
    #[serde(default)]
    env:     Option<String>,
    #[serde(default)]
    vault:   Option<String>,
}

impl<'de> Deserialize<'de> for HeaderValueRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error as _;

        match HeaderValueRefInput::deserialize(deserializer)? {
            HeaderValueRefInput::BareString(value) => {
                drop(value);
                Err(D::Error::custom("header value must be a table"))
            }
            HeaderValueRefInput::Table(value) => value.try_into().map_err(D::Error::custom),
        }
    }
}

impl TryFrom<HeaderValueRefSerde> for HeaderValueRef {
    type Error = HeaderValueRefParseError;

    fn try_from(value: HeaderValueRefSerde) -> Result<Self, Self::Error> {
        let populated = [
            value.literal.as_ref(),
            value.env.as_ref(),
            value.vault.as_ref(),
        ]
        .into_iter()
        .flatten()
        .count();
        if populated != 1 {
            return Err(HeaderValueRefParseError::WrongFieldCount);
        }
        if let Some(value) = value.literal {
            return non_empty_header_value(value).map(Self::Literal);
        }
        if let Some(value) = value.env {
            return non_empty_header_value(value).map(Self::Env);
        }
        if let Some(value) = value.vault {
            return non_empty_header_value(value).map(Self::Vault);
        }
        unreachable!("populated field count was already checked");
    }
}

fn non_empty_header_value(value: String) -> Result<String, HeaderValueRefParseError> {
    if value.is_empty() {
        Err(HeaderValueRefParseError::EmptyValue)
    } else {
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HeaderValueRefParseError {
    #[error("header value must contain exactly one of `literal`, `env`, or `vault`")]
    WrongFieldCount,
    #[error("header value reference must not be empty")]
    EmptyValue,
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
    pub provider: String,
    pub model:    String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogProvider {
    pub id:             ProviderId,
    pub display_name:   String,
    pub adapter:        AdapterKind,
    pub agent_profile:  AgentProfileKind,
    pub auth:           Option<ProviderAuthConfig>,
    pub billing_policy: BillingPolicy,
    pub api_key_url:    Option<String>,
    pub base_url:       Option<String>,
    pub extra_headers:  HashMap<String, HeaderValueRef>,
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
                CredentialRef::Env(_) => None,
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
    pub api_id:        String,
    pub agent_profile: AgentProfileKind,
    pub controls:      CatalogModelControls,
    pub speed_costs:   HashMap<Speed, ModelCosts>,
    probe:             bool,
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
    #[error("provider '{provider}' API-key auth must declare at least one credential")]
    EmptyApiKeyCredentials { provider: ProviderId },
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
    #[error("model identifier '{identifier}' is declared by both '{first}' and '{second}'")]
    DuplicateModelIdentifier {
        identifier: String,
        first:      String,
        second:     String,
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

/// Typed model catalog backed by a `Vec<Model>`.
///
/// Use [`Catalog::builtin()`] for the embedded settings-backed catalog.
#[derive(Debug)]
pub struct Catalog {
    models:           Vec<Model>,
    providers:        Vec<CatalogProvider>,
    model_settings:   HashMap<String, CatalogModelSettings>,
    model_index:      HashMap<String, usize>,
    provider_aliases: HashMap<String, ProviderId>,
    provider_index:   HashMap<ProviderId, usize>,
}

impl Catalog {
    /// Returns a reference to the global built-in catalog (loaded once from
    /// embedded provider TOML files).
    #[must_use]
    pub fn builtin() -> &'static Self {
        &GLOBAL_CATALOG
    }

    pub fn from_settings(settings: &LlmCatalogSettings) -> Result<Self, CatalogBuildError> {
        let mut providers = build_providers(settings)?;
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
        let mut model_identifiers = BTreeMap::<String, String>::new();
        let mut defaults_by_provider = HashMap::<ProviderId, Vec<String>>::new();
        let mut small_defaults_by_provider = HashMap::<ProviderId, Vec<String>>::new();

        let mut model_ids = settings.models.keys().cloned().collect::<Vec<_>>();
        model_ids.sort_unstable();
        for model_id in model_ids {
            let model_settings = settings
                .models
                .get(&model_id)
                .expect("model ID came from settings map keys");
            if model_settings.enabled == Some(false) {
                continue;
            }

            let provider_id =
                required_model_string(&model_id, model_settings.provider.as_ref(), "provider")?;
            if !known_providers.contains(provider_id.as_str()) {
                return Err(CatalogBuildError::UnknownModelProvider {
                    model:    model_id,
                    provider: ProviderId::from(provider_id),
                });
            }
            if !enabled_providers.contains(provider_id.as_str()) {
                continue;
            }

            let provider = provider_by_id
                .get(provider_id.as_str())
                .expect("enabled provider ID should have provider metadata");
            let (model, resolved_settings) = build_model(&model_id, model_settings, provider)?;

            register_model_identifier(&mut model_identifiers, model.id.clone(), model.id.clone())?;
            for alias in &model.aliases {
                register_model_identifier(&mut model_identifiers, alias.clone(), model.id.clone())?;
            }

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

        for (provider, defaults) in defaults_by_provider {
            if defaults.len() > 1 {
                return Err(CatalogBuildError::MultipleProviderDefaults {
                    provider,
                    models: defaults,
                });
            }
        }
        for (provider, small_defaults) in small_defaults_by_provider {
            if small_defaults.len() > 1 {
                return Err(CatalogBuildError::MultipleProviderSmallDefaults {
                    provider,
                    models: small_defaults,
                });
            }
        }
        if !models_with_settings.iter().any(|(model, _)| model.default) {
            return Err(CatalogBuildError::NoDefaultModel);
        }

        models_with_settings.sort_by(|(left, _), (right, _)| model_order(left, right));
        warn_multiple_probe_models(&models_with_settings);
        let mut model_settings_by_id = HashMap::new();
        let mut models = Vec::new();
        for (model, settings) in models_with_settings {
            model_settings_by_id.insert(model.id.clone(), settings);
            models.push(model);
        }
        let model_index = build_model_index(&models);

        Ok(Self {
            models,
            providers,
            model_settings: model_settings_by_id,
            model_index,
            provider_aliases,
            provider_index,
        })
    }

    pub fn from_builtin_with_overrides(
        overrides: &LlmCatalogSettings,
    ) -> Result<Self, CatalogBuildError> {
        let builtins = Self::builtin_settings()?;
        let settings = merge_catalog_settings(overrides.clone(), builtins);
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

        Ok(layer)
    }

    fn from_builtin_toml() -> Result<Self, CatalogBuildError> {
        Self::from_settings(&Self::builtin_settings()?)
    }

    /// Look up a model by ID or alias.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Model> {
        self.model_index
            .get(id)
            .and_then(|idx| self.models.get(*idx))
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
                stats.default_model = Some(model.id.clone());
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
    pub fn model_settings(&self, id: &str) -> Option<&CatalogModelSettings> {
        let model = self.get(id)?;
        self.model_settings.get(&model.id)
    }

    #[must_use]
    pub fn effective_agent_profile(
        &self,
        provider_id: &ProviderId,
        model_id_or_alias: Option<&str>,
    ) -> Option<AgentProfileKind> {
        let provider = self.provider(provider_id)?;
        let model_profile = model_id_or_alias
            .and_then(|model_id| self.get(model_id))
            .filter(|model| model.provider == provider.id)
            .and_then(|model| self.model_settings.get(&model.id))
            .map(|settings| settings.agent_profile);
        Some(model_profile.unwrap_or(provider.agent_profile))
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
    /// for metadata enrichment. Falls back to the provider's normal default
    /// when no explicit small default is configured.
    #[must_use]
    pub fn small_default_for_provider(&self, p: &ProviderId) -> Option<&Model> {
        let provider_id = self.provider(p).map_or(p, |provider| &provider.id);
        self.models
            .iter()
            .find(|m| &m.provider == provider_id && m.small_default)
            .or_else(|| self.default_for_provider(provider_id))
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
        let configured = configured
            .iter()
            .filter_map(|id| self.provider(id).map(|provider| provider.id.clone()))
            .collect::<HashSet<_>>();
        self.providers
            .iter()
            .filter(|provider| configured.contains(&provider.id))
            .find_map(|provider| self.default_for_provider(&provider.id))
            .unwrap_or_else(|| self.default_model())
    }

    /// Small default model for the best-available built-in provider IDs,
    /// falling back to the global catalog default.
    #[must_use]
    pub fn small_default_for_configured_ids(&self, configured: &[ProviderId]) -> &Model {
        if configured.is_empty() {
            return self.default_model();
        }
        let configured = configured
            .iter()
            .filter_map(|id| self.provider(id).map(|provider| provider.id.clone()))
            .collect::<HashSet<_>>();
        self.providers
            .iter()
            .filter(|provider| configured.contains(&provider.id))
            .find_map(|provider| self.small_default_for_provider(&provider.id))
            .unwrap_or_else(|| self.default_model())
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
                    .model_settings
                    .get(&model.id)
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

    /// Build an ordered fallback chain for a primary provider/model.
    ///
    /// For each fallback provider, finds the closest matching model. Providers
    /// where no capability match exists (or the provider string doesn't
    /// parse) are skipped.
    #[must_use]
    pub fn build_fallback_chain(
        &self,
        primary: &ProviderId,
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
                let provider = ProviderId::from(provider_str.clone());
                self.closest(&provider, reference).map(|m| FallbackTarget {
                    provider: provider_str.clone(),
                    model:    m.id.clone(),
                })
            })
            .collect()
    }
}

fn build_model_index(models: &[Model]) -> HashMap<String, usize> {
    let mut index = HashMap::new();
    for (idx, model) in models.iter().enumerate() {
        index.insert(model.id.clone(), idx);
        for alias in &model.aliases {
            index.insert(alias.clone(), idx);
        }
    }
    index
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

    for (id, model) in higher.models {
        let model = match fallback.models.remove(&id) {
            Some(fallback_model) => merge_model_settings(model, fallback_model),
            None => model,
        };
        fallback.models.insert(id, model);
    }

    fallback
}

fn merge_provider_settings(
    higher: ProviderCatalogSettings,
    fallback: ProviderCatalogSettings,
) -> ProviderCatalogSettings {
    ProviderCatalogSettings {
        display_name:   higher.display_name.or(fallback.display_name),
        adapter:        higher.adapter.or(fallback.adapter),
        agent_profile:  higher.agent_profile.or(fallback.agent_profile),
        auth:           higher.auth.or(fallback.auth),
        billing_policy: higher.billing_policy.or(fallback.billing_policy),
        api_key_url:    higher.api_key_url.or(fallback.api_key_url),
        base_url:       higher.base_url.or(fallback.base_url),
        extra_headers:  higher.extra_headers.or(fallback.extra_headers),
        priority:       higher.priority.or(fallback.priority),
        enabled:        higher.enabled.or(fallback.enabled),
        aliases:        higher.aliases.or(fallback.aliases),
    }
}

fn merge_model_settings(
    higher: ModelCatalogSettings,
    fallback: ModelCatalogSettings,
) -> ModelCatalogSettings {
    ModelCatalogSettings {
        provider:             higher.provider.or(fallback.provider),
        api_id:               higher.api_id.or(fallback.api_id),
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
        tools:            higher.tools.or(fallback.tools),
        vision:           higher.vision.or(fallback.vision),
        reasoning:        higher.reasoning.or(fallback.reasoning),
        reasoning_effort: higher.reasoning_effort.or(fallback.reasoning_effort),
        prompt_cache:     higher.prompt_cache.or(fallback.prompt_cache),
        sampling_params:  higher.sampling_params.or(fallback.sampling_params),
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
        let agent_profile = settings.agent_profile.unwrap_or(defaults.agent_profile);
        let auth = settings.auth.clone();
        validate_provider_auth(&provider_id, auth.as_ref())?;

        providers.push(CatalogProvider {
            id: provider_id,
            display_name: settings.display_name.clone().unwrap_or_else(|| id.clone()),
            adapter,
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
        AdapterKind::Anthropic => AdapterDefaults {
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

fn validate_provider_auth(
    provider: &ProviderId,
    auth: Option<&ProviderAuthConfig>,
) -> Result<(), CatalogBuildError> {
    match auth {
        Some(auth) if auth.credentials.is_empty() => {
            Err(CatalogBuildError::EmptyApiKeyCredentials {
                provider: provider.clone(),
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
    let controls = build_model_controls(model_id, &model_features, settings)?;
    let costs = build_model_costs(settings.costs.as_ref());
    let speed_costs = build_speed_costs(model_id, settings.costs.as_ref(), &controls)?;

    let model = Model {
        id: model_id.to_string(),
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
        costs,
        estimated_output_tps: settings.estimated_output_tps,
        aliases: settings.aliases.clone().unwrap_or_default(),
        default: settings.default.unwrap_or_default(),
        small_default: settings.small_default.unwrap_or_default(),
        configured: false,
    };
    let catalog_settings = CatalogModelSettings {
        api_id: settings
            .api_id
            .clone()
            .unwrap_or_else(|| model_id.to_string()),
        agent_profile: settings.agent_profile.unwrap_or(provider.agent_profile),
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
                .push(model.id.clone());
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
        prompt_cache: features.prompt_cache.unwrap_or_default(),
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
    identifiers: &mut BTreeMap<String, String>,
    identifier: String,
    owner: String,
) -> Result<(), CatalogBuildError> {
    match identifiers.get(&identifier) {
        Some(existing) if existing != &owner => Err(CatalogBuildError::DuplicateModelIdentifier {
            identifier,
            first: existing.clone(),
            second: owner,
        }),
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

fn model_order(left: &Model, right: &Model) -> std::cmp::Ordering {
    left.provider
        .cmp(&right.provider)
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
        assert_eq!(m.id, "claude-sonnet-4-6");
        assert!(m.default);

        let m = Catalog::builtin()
            .default_for_provider(&ProviderId::openai())
            .unwrap();
        assert_eq!(m.id, "gpt-5.5");

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
    fn builtin_build_fallback_chain() {
        let fallbacks = HashMap::from([("anthropic".to_string(), vec![
            "gemini".to_string(),
            "openai".to_string(),
        ])]);
        let chain = Catalog::builtin().build_fallback_chain(
            &ProviderId::anthropic(),
            "claude-opus-4-6",
            &fallbacks,
        );
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].provider, "gemini");
        assert_eq!(chain[0].model, "gemini-3.1-pro-preview");
        assert_eq!(chain[1].provider, "openai");
        assert_eq!(chain[1].model, "gpt-5.5");
    }

    #[test]
    fn builtin_build_fallback_chain_unknown_model() {
        let fallbacks = HashMap::from([("anthropic".to_string(), vec!["gemini".to_string()])]);
        let chain = Catalog::builtin().build_fallback_chain(
            &ProviderId::anthropic(),
            "unknown-xyz",
            &fallbacks,
        );
        assert!(chain.is_empty());
    }

    #[test]
    fn builtin_build_fallback_chain_provider_not_in_map() {
        let fallbacks = HashMap::from([("openai".to_string(), vec!["anthropic".to_string()])]);
        let chain = Catalog::builtin().build_fallback_chain(
            &ProviderId::anthropic(),
            "claude-opus-4-6",
            &fallbacks,
        );
        assert!(chain.is_empty());
    }

    #[test]
    fn builtin_build_fallback_chain_skips_no_capability_match() {
        let fallbacks = HashMap::from([("anthropic".to_string(), vec![
            "openai".to_string(),
            "kimi".to_string(),
        ])]);
        let chain = Catalog::builtin().build_fallback_chain(
            &ProviderId::anthropic(),
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
            &ProviderId::anthropic(),
            "claude-opus-4-6",
            &fallbacks,
        );
        assert!(chain.is_empty());
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
                .provider(&ProviderId::new("kimi"))
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

    #[test]
    fn catalog_from_settings_rejects_duplicate_model_aliases() {
        let layer = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"
enabled = true

[models.one]
provider = "test"
display_name = "One"
family = "test"
aliases = ["shared"]

[models.one.limits]
context_window = 1000

[models.one.features]
tools = false
vision = false
reasoning = false

[models.two]
provider = "test"
display_name = "Two"
family = "test"
aliases = ["shared"]

[models.two.limits]
context_window = 1000

[models.two.features]
tools = false
vision = false
reasoning = false
"#,
        );

        let err = Catalog::from_settings(&layer).unwrap_err();

        assert!(matches!(
            err,
            CatalogBuildError::DuplicateModelIdentifier { identifier, first, second }
                if identifier == "shared" && first == "one" && second == "two"
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
    fn catalog_lists_models_by_provider_then_model_id() {
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

        assert_eq!(ids, ["alpha_one", "zeta_one", "zeta_two"]);
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

        assert_eq!(
            catalog
                .probe_for_provider(&ProviderId::openai())
                .unwrap()
                .id,
            "gpt-5.5"
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
    fn small_default_for_provider_falls_back_to_provider_default_when_no_small_default_marked() {
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
                .small_default_for_provider(&ProviderId::new("test"))
                .unwrap()
                .id,
            "default_model"
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

        assert_eq!(
            catalog
                .small_default_for_provider(&ProviderId::openai())
                .unwrap()
                .id,
            "gpt-5.5"
        );
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
                sampling_params: true,
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
            provider: kimi,
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
                reasoning_effort: None,
                prompt_cache: false,
                sampling_params: true,
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
            small_default: false,
            configured: false,
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
        assert_eq!(m.provider, ProviderId::new("zai"));
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
                sampling_params: true,
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
                sampling_params: true,
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
[models."gpt-5.5".limits]
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
    fn closest_model_haiku_to_kimi() {
        let haiku = Catalog::builtin().get("claude-haiku-4-5").unwrap();
        let result = Catalog::builtin()
            .closest(&ProviderId::new("kimi"), haiku)
            .unwrap();
        assert_eq!(result.id, "kimi-k2.5");
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
