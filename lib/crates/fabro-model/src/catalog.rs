use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;
use std::sync::LazyLock;

use rust_embed::RustEmbed;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use strum::VariantArray;
use toml::de::Error as TomlDeError;

use crate::Speed;
use crate::adapter::{self, AdapterMetadata};
use crate::ids::ProviderId;
use crate::provider::Provider;
use crate::reasoning::ReasoningEffort;
use crate::types::{Model, ModelCosts, ModelFeatures, ModelLimits};

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
    pub display_name:  Option<String>,
    #[serde(default)]
    pub adapter:       Option<String>,
    #[serde(default)]
    pub base_url:      Option<String>,
    #[serde(default)]
    pub credentials:   Option<Vec<CredentialRef>>,
    #[serde(default)]
    pub extra_headers: Option<HashMap<String, HeaderValueRef>>,
    #[serde(default)]
    pub priority:      Option<i32>,
    #[serde(default)]
    pub enabled:       Option<bool>,
    #[serde(default)]
    pub aliases:       Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCatalogSettings {
    #[serde(default)]
    pub provider:             Option<String>,
    #[serde(default)]
    pub api_id:               Option<String>,
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
    pub tools:     Option<bool>,
    #[serde(default)]
    pub vision:    Option<bool>,
    #[serde(default)]
    pub reasoning: Option<bool>,
    #[serde(default)]
    pub effort:    Option<bool>,
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
    Credential(String),
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

impl From<CredentialRef> for String {
    fn from(value: CredentialRef) -> Self {
        value.to_string()
    }
}

impl FromStr for CredentialRef {
    type Err = CredentialRefParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some(id) = value.strip_prefix("credential:") {
            if id.is_empty() {
                return Err(CredentialRefParseError::EmptyCredential);
            }
            return Ok(Self::Credential(id.to_string()));
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
    #[error("credential reference must be `credential:<id>` or `env:<NAME>`")]
    Invalid,
    #[error("credential reference is missing an ID after `credential:`")]
    EmptyCredential,
    #[error("credential reference is missing a name after `env:`")]
    EmptyEnv,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderValueRef {
    Literal(String),
    Env(String),
    Credential(String),
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
            Self::Credential(value) => map.serialize_entry("credential", value)?,
        }
        map.end()
    }
}

impl std::fmt::Display for HeaderValueRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Literal(_) => f.write_str("literal:<redacted>"),
            Self::Env(name) => write!(f, "env:{name}"),
            Self::Credential(id) => write!(f, "credential:{id}"),
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
    literal:    Option<String>,
    #[serde(default)]
    env:        Option<String>,
    #[serde(default)]
    credential: Option<String>,
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
            value.credential.as_ref(),
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
        if let Some(value) = value.credential {
            return non_empty_header_value(value).map(Self::Credential);
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
    #[error("header value must contain exactly one of `literal`, `env`, or `credential`")]
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
    pub id:            ProviderId,
    pub display_name:  String,
    pub adapter:       String,
    pub base_url:      Option<String>,
    pub credentials:   Vec<CredentialRef>,
    pub extra_headers: HashMap<String, HeaderValueRef>,
    pub priority:      i32,
    pub aliases:       Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogModelControls {
    pub reasoning_effort: Vec<ReasoningEffort>,
    pub speed:            Vec<Speed>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogModelSettings {
    pub api_id:      String,
    pub controls:    CatalogModelControls,
    pub speed_costs: HashMap<Speed, ModelCosts>,
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
    #[error("provider '{provider}' uses adapter 'openai_compatible' without base_url")]
    MissingOpenAiCompatibleBaseUrl { provider: ProviderId },
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
    #[error("catalog must contain at least one enabled default model")]
    NoDefaultModel,
    #[error("model '{model}' has invalid reasoning_effort '{value}'")]
    InvalidReasoningEffort {
        model:  String,
        value:  String,
        #[source]
        source: strum::ParseError,
    },
    #[error(
        "model '{model}' declares unsupported reasoning_effort '{value}' for adapter '{adapter}'"
    )]
    UnsupportedReasoningEffort {
        model:   String,
        adapter: String,
        value:   ReasoningEffort,
    },
    #[error("model '{model}' declares reasoning_effort controls but features.effort is false")]
    ReasoningEffortWithoutFeature { model: String },
    #[error(
        "model '{model}' must declare at least one reasoning_effort when features.effort is true"
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
    #[error("model '{model}' declares unsupported speed '{speed}' for adapter '{adapter}'")]
    UnsupportedSpeed {
        model:   String,
        adapter: String,
        speed:   Speed,
    },
    #[error("model '{model}' has costs.speed.{speed} without declaring controls.speed")]
    UndeclaredSpeedCost { model: String, speed: Speed },
}

/// Typed model catalog backed by a `Vec<Model>`.
///
/// Use [`Catalog::builtin()`] for the embedded settings-backed catalog, or
/// [`Catalog::from_models()`] for testing with custom model sets.
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
        if !models_with_settings.iter().any(|(model, _)| model.default) {
            return Err(CatalogBuildError::NoDefaultModel);
        }

        models_with_settings.sort_by(|(left, _), (right, _)| model_order(left, right));
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

    /// Create a catalog from a custom set of models (useful for testing).
    #[must_use]
    pub fn from_models(models: Vec<Model>) -> Self {
        let mut providers = Vec::<CatalogProvider>::new();
        let mut seen = HashSet::<ProviderId>::new();
        for model in &models {
            if !seen.insert(model.provider.clone()) {
                continue;
            }
            providers.push(CatalogProvider {
                id:            model.provider.clone(),
                display_name:  Provider::display_name_for_id(&model.provider),
                adapter:       default_adapter_for_provider(&model.provider).to_string(),
                base_url:      None,
                credentials:   Vec::new(),
                extra_headers: HashMap::new(),
                priority:      0,
                aliases:       Vec::new(),
            });
        }

        let provider_index = providers
            .iter()
            .enumerate()
            .map(|(idx, provider)| (provider.id.clone(), idx))
            .collect::<HashMap<_, _>>();
        let model_settings = models
            .iter()
            .map(|model| {
                (model.id.clone(), CatalogModelSettings {
                    api_id:      model.id.clone(),
                    controls:    default_controls_for_model(model),
                    speed_costs: HashMap::new(),
                })
            })
            .collect();
        let model_index = build_model_index(&models);

        Self {
            models,
            providers,
            model_settings,
            model_index,
            provider_aliases: HashMap::new(),
            provider_index,
        }
    }

    fn from_builtin_toml() -> Result<Self, CatalogBuildError> {
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

        Self::from_settings(&layer)
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
    pub fn provider(&self, id: &ProviderId) -> Option<&CatalogProvider> {
        let canonical = self.provider_aliases.get(id.as_str()).unwrap_or(id);
        self.provider_index
            .get(canonical)
            .and_then(|idx| self.providers.get(*idx))
    }

    #[must_use]
    pub fn model_settings(&self, id: &str) -> Option<&CatalogModelSettings> {
        let model = self.get(id)?;
        self.model_settings.get(&model.id)
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
                provider.credentials.iter().any(|credential| {
                    matches!(credential, CredentialRef::Env(name) if std::env::var(name).is_ok())
                })
            })
            .map(|provider| provider.id.clone())
            .collect::<Vec<_>>();
        self.default_for_configured_ids(&configured)
    }

    /// Default model for the best configured provider, falling back to the
    /// global catalog default.
    #[must_use]
    pub fn default_for_configured(&self, configured: &[Provider]) -> &Model {
        let configured = configured
            .iter()
            .copied()
            .map(Provider::id)
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

    /// Probe model for a provider — the cheapest model suitable for
    /// connectivity checks. Falls back to the provider's default when no
    /// explicit override is configured.
    #[must_use]
    pub fn probe_for_provider(&self, p: Provider) -> Option<&Model> {
        let override_id: Option<&str> = match p {
            Provider::Anthropic => Some("claude-haiku-4-5"),
            Provider::OpenAi => Some("gpt-5.4-mini"),
            _ => None,
        };
        if let Some(id) = override_id {
            if let Some(info) = self.get(id) {
                return Some(info);
            }
        }
        self.default_for_provider(&p.id())
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

        let adapter = required_provider_string(&provider_id, settings.adapter.as_ref(), "adapter")?;
        if adapter::get(adapter.as_str()).is_none() {
            return Err(CatalogBuildError::UnknownAdapter {
                provider: provider_id,
                adapter,
            });
        }
        if adapter == adapter::OPENAI_COMPATIBLE.key && settings.base_url.is_none() {
            return Err(CatalogBuildError::MissingOpenAiCompatibleBaseUrl {
                provider: provider_id,
            });
        }

        providers.push(CatalogProvider {
            id: provider_id,
            display_name: settings.display_name.clone().unwrap_or_else(|| id.clone()),
            adapter,
            base_url: settings.base_url.clone(),
            credentials: settings.credentials.clone().unwrap_or_default(),
            extra_headers: settings.extra_headers.clone().unwrap_or_default(),
            priority: settings.priority.unwrap_or_default(),
            aliases: settings.aliases.clone().unwrap_or_default(),
        });
    }
    Ok(providers)
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
    let adapter = adapter::get(&provider.adapter).expect("provider adapter was validated earlier");
    let controls = build_model_controls(model_id, &model_features, settings, adapter)?;
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
        configured: false,
    };
    let catalog_settings = CatalogModelSettings {
        api_id: settings
            .api_id
            .clone()
            .unwrap_or_else(|| model_id.to_string()),
        controls,
        speed_costs,
    };
    Ok((model, catalog_settings))
}

fn build_model_features(
    model_id: &str,
    features: &SettingsModelFeatures,
) -> Result<ModelFeatures, CatalogBuildError> {
    Ok(ModelFeatures {
        tools:     features
            .tools
            .ok_or_else(|| CatalogBuildError::MissingModelField {
                model: model_id.to_string(),
                field: "features.tools",
            })?,
        vision:    features
            .vision
            .ok_or_else(|| CatalogBuildError::MissingModelField {
                model: model_id.to_string(),
                field: "features.vision",
            })?,
        reasoning: features
            .reasoning
            .ok_or_else(|| CatalogBuildError::MissingModelField {
                model: model_id.to_string(),
                field: "features.reasoning",
            })?,
        effort:    features.effort.unwrap_or_default(),
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
    adapter: &'static AdapterMetadata,
) -> Result<CatalogModelControls, CatalogBuildError> {
    let reasoning_effort = match settings
        .controls
        .as_ref()
        .and_then(|controls| controls.reasoning_effort.as_ref())
    {
        Some(values) if !features.effort && !values.is_empty() => {
            return Err(CatalogBuildError::ReasoningEffortWithoutFeature {
                model: model_id.to_string(),
            });
        }
        Some(values) if values.is_empty() && features.effort => {
            return Err(CatalogBuildError::EmptyReasoningEffortControls {
                model: model_id.to_string(),
            });
        }
        Some(values) => values
            .iter()
            .map(|value| parse_reasoning_effort(model_id, value))
            .collect::<Result<Vec<_>, _>>()?,
        None if features.effort => adapter.controls.native_reasoning_effort.to_vec(),
        None => Vec::new(),
    };
    for value in &reasoning_effort {
        if !adapter.controls.native_reasoning_effort.contains(value) {
            return Err(CatalogBuildError::UnsupportedReasoningEffort {
                model:   model_id.to_string(),
                adapter: adapter.key.to_string(),
                value:   *value,
            });
        }
    }

    let speed = settings
        .controls
        .as_ref()
        .and_then(|controls| controls.speed.as_ref())
        .map(|values| {
            values
                .iter()
                .map(|value| parse_speed_control(model_id, value, adapter))
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

fn parse_speed_control(
    model_id: &str,
    value: &str,
    adapter: &'static AdapterMetadata,
) -> Result<Speed, CatalogBuildError> {
    let speed = parse_speed(model_id, value)?;
    if speed == Speed::Standard {
        return Err(CatalogBuildError::StandardSpeedControl {
            model: model_id.to_string(),
        });
    }
    if !adapter.controls.additional_speeds.contains(&speed) {
        return Err(CatalogBuildError::UnsupportedSpeed {
            model: model_id.to_string(),
            adapter: adapter.key.to_string(),
            speed,
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

fn default_adapter_for_provider(provider: &ProviderId) -> &'static str {
    match Provider::from_id(provider) {
        Some(Provider::Anthropic) => adapter::ANTHROPIC.key,
        Some(Provider::OpenAi) => adapter::OPENAI.key,
        Some(Provider::Gemini) => adapter::GEMINI.key,
        Some(
            Provider::Kimi
            | Provider::Zai
            | Provider::Minimax
            | Provider::Inception
            | Provider::OpenAiCompatible,
        )
        | None => adapter::OPENAI_COMPATIBLE.key,
    }
}

fn default_controls_for_model(model: &Model) -> CatalogModelControls {
    CatalogModelControls {
        reasoning_effort: if model.features.effort {
            ReasoningEffort::VARIANTS.to_vec()
        } else {
            Vec::new()
        },
        speed:            Vec::new(),
    }
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
    use std::str::FromStr;

    use strum::VariantArray;

    use super::*;
    use crate::Speed;
    use crate::provider::Provider;
    use crate::reasoning::ReasoningEffort;

    fn minimal_settings(source: &str) -> LlmCatalogSettings {
        toml::from_str(source).expect("fixture should parse as an LLM settings layer")
    }

    // ---- Catalog struct tests ----

    #[test]
    fn builtin_get_by_id() {
        let m = Catalog::builtin().get("claude-opus-4-6").unwrap();
        assert_eq!(m.id, "claude-opus-4-6");
    }

    #[test]
    fn builtin_get_by_alias() {
        let m = Catalog::builtin().get("opus").unwrap();
        assert_eq!(m.id, "claude-opus-4-7");
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
        let anthropic = Catalog::builtin().list(Some(&Provider::Anthropic.id()));
        assert!(!anthropic.is_empty());
        assert!(
            anthropic
                .iter()
                .all(|m| m.provider == Provider::Anthropic.id())
        );
    }

    #[test]
    fn builtin_list_unknown_provider_empty() {
        // OpenAiCompatible has no catalog models
        let models = Catalog::builtin().list(Some(&Provider::OpenAiCompatible.id()));
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
            .default_for_provider(&Provider::Anthropic.id())
            .unwrap();
        assert_eq!(m.id, "claude-sonnet-4-6");
        assert!(m.default);

        let m = Catalog::builtin()
            .default_for_provider(&Provider::OpenAi.id())
            .unwrap();
        assert_eq!(m.id, "gpt-5.4");

        let m = Catalog::builtin()
            .default_for_provider(&Provider::Gemini.id())
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
    fn builtin_probe_anthropic_returns_override() {
        let m = Catalog::builtin()
            .probe_for_provider(Provider::Anthropic)
            .unwrap();
        assert_eq!(m.id, "claude-haiku-4-5");
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
        let result = Catalog::builtin()
            .closest(&Provider::Gemini.id(), opus)
            .unwrap();
        assert_eq!(result.id, "gemini-3.1-pro-preview");
    }

    #[test]
    fn builtin_closest_no_match() {
        let haiku = Catalog::builtin().get("claude-haiku-4-5").unwrap();
        assert!(
            Catalog::builtin()
                .closest(&Provider::OpenAi.id(), haiku)
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
            &Provider::Anthropic.id(),
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
            &Provider::Anthropic.id(),
            "unknown-xyz",
            &fallbacks,
        );
        assert!(chain.is_empty());
    }

    #[test]
    fn builtin_build_fallback_chain_provider_not_in_map() {
        let fallbacks = HashMap::from([("openai".to_string(), vec!["anthropic".to_string()])]);
        let chain = Catalog::builtin().build_fallback_chain(
            &Provider::Anthropic.id(),
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
            &Provider::Anthropic.id(),
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
            &Provider::Anthropic.id(),
            "claude-opus-4-6",
            &fallbacks,
        );
        assert!(chain.is_empty());
    }

    #[test]
    fn from_models_custom_catalog() {
        use crate::types::{Model, ModelCosts, ModelFeatures, ModelLimits};

        let models = vec![Model {
            id:                   "test-model".to_string(),
            provider:             Provider::Anthropic.id(),
            family:               "test".to_string(),
            display_name:         "Test Model".to_string(),
            limits:               ModelLimits {
                context_window: 100_000,
                max_output:     Some(4096),
            },
            training:             None,
            knowledge_cutoff:     None,
            features:             ModelFeatures {
                tools:     true,
                vision:    false,
                reasoning: false,
                effort:    false,
            },
            costs:                ModelCosts {
                input_cost_per_mtok:       Some(1.0),
                output_cost_per_mtok:      Some(5.0),
                cache_input_cost_per_mtok: None,
            },
            estimated_output_tps: None,
            aliases:              vec!["test".to_string()],
            default:              true,
            configured:           false,
        }];

        let catalog = Catalog::from_models(models);
        assert_eq!(catalog.get("test-model").unwrap().id, "test-model");
        assert_eq!(catalog.get("test").unwrap().id, "test-model");
        assert!(catalog.get("nonexistent").is_none());
        assert_eq!(catalog.default_model().id, "test-model");
        assert_eq!(catalog.list(None).len(), 1);
    }

    #[test]
    fn builtin_catalog_is_loaded_from_provider_toml_settings() {
        let catalog = Catalog::builtin();

        assert_eq!(
            catalog.provider(&ProviderId::openai()).unwrap().adapter,
            "openai"
        );
        assert_eq!(
            catalog
                .provider(&ProviderId::kimi())
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
    }

    #[test]
    fn catalog_from_settings_rejects_unknown_adapter() {
        let layer = minimal_settings(
            r#"
[providers.venice]
display_name = "Venice"
adapter = "not_real"
enabled = true
"#,
        );

        let err = Catalog::from_settings(&layer).unwrap_err();

        assert!(matches!(
            err,
            CatalogBuildError::UnknownAdapter { provider, adapter }
                if provider == ProviderId::new("venice") && adapter == "not_real"
        ));
    }

    #[test]
    fn catalog_from_settings_rejects_duplicate_model_aliases() {
        let layer = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"
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
priority = 10

[providers.high]
display_name = "High"
adapter = "openai"
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
priority = 20

[providers.alpha]
display_name = "Alpha"
adapter = "openai"
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
    fn catalog_from_settings_validates_model_controls_and_speed_costs() {
        let invalid_effort = minimal_settings(
            r#"
[providers.test]
display_name = "Test"
adapter = "openai"

[models.model]
provider = "test"
display_name = "Model"
family = "test"

[models.model.limits]
context_window = 1000

[models.model.features]
tools = false
vision = false
reasoning = true
effort = true

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

[models.model]
provider = "test"
display_name = "Model"
family = "test"

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

    // ---- Provider / catalog data integrity tests ----

    #[test]
    fn every_provider_has_catalog_models() {
        for &provider in Provider::ALL {
            let models = Catalog::builtin().list(Some(&provider.id()));
            assert!(
                !models.is_empty(),
                "Provider {provider:?} has no models in catalog"
            );
        }
    }

    #[test]
    fn every_provider_has_exactly_one_default_model() {
        for &provider in Provider::ALL {
            let defaults: Vec<_> = Catalog::builtin()
                .list(Some(&provider.id()))
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
    fn catalog_providers_roundtrip_through_static_str() {
        for model in Catalog::builtin().list(None) {
            let roundtripped = Provider::from_id(&model.provider);
            assert_eq!(
                roundtripped,
                Some(
                    model
                        .builtin_provider()
                        .expect("catalog model provider should be a built-in provider")
                ),
                "catalog model '{}' provider {:?} does not roundtrip through ProviderId",
                model.id,
                model.provider
            );
        }
    }

    #[test]
    fn provider_static_str_roundtrips_through_from_str() {
        for &provider in Provider::ALL {
            let roundtripped = Provider::from_str(<&'static str>::from(provider));
            assert_eq!(
                roundtripped,
                Ok(provider),
                "Provider::{provider:?} IntoStaticStr does not round-trip through from_str"
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
                effort: true,
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
            configured: false,
        }
        "#);
    }

    #[test]
    fn get_model_info_by_alias() {
        assert_eq!(
            Catalog::builtin().get("opus").unwrap().id,
            "claude-opus-4-7"
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
            provider: gemini,
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
            configured: false,
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
        assert_eq!(m.provider, Provider::Zai.id());
    }

    #[test]
    fn minimax_m2_5_in_catalog() {
        let m = Catalog::builtin().get("minimax-m2.5").unwrap();
        assert_eq!(m.provider, Provider::Minimax.id());
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
            configured: false,
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
            provider: openai,
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
            configured: false,
        }
        "#);
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
    fn gpt_5_3_codex_spark_in_catalog() {
        let m = Catalog::builtin().get("gpt-5.3-codex-spark").unwrap();
        insta::assert_debug_snapshot!(m, @r#"
        Model {
            id: "gpt-5.3-codex-spark",
            provider: openai,
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
            configured: false,
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
            .closest(&Provider::Gemini.id(), sonnet)
            .unwrap();
        assert_eq!(result.id, "gemini-3.1-pro-preview");
    }

    #[test]
    fn closest_model_haiku_to_kimi() {
        let haiku = Catalog::builtin().get("claude-haiku-4-5").unwrap();
        let result = Catalog::builtin()
            .closest(&Provider::Kimi.id(), haiku)
            .unwrap();
        assert_eq!(result.id, "kimi-k2.5");
    }

    #[test]
    fn closest_model_no_capability_match() {
        let glm = Catalog::builtin().get("glm-4.7").unwrap();
        assert!(
            Catalog::builtin()
                .closest(&Provider::Gemini.id(), glm)
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
