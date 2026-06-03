use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use fabro_model::catalog as model_catalog;
use fabro_types::settings::{RunNamespace, WorkflowNamespace};
use fabro_types::{ServerSettings, UserSettings, WorkflowSettings};
use fabro_util::error::SharedError;

use crate::defaults::DEFAULTS_LAYER;
use crate::load::load_settings_path;
use crate::parse::{SettingsSource, validate_settings_source};
use crate::resolve::{
    ResolveError, resolve_cli, resolve_project, resolve_run, resolve_server, resolve_workflow,
};
use crate::user::load_settings_config;
use crate::{
    CliLayer, Combine, CostRates, EnvironmentLayer, Error, LlmLayer, LlmModelFeatures,
    LlmModelLimits, MergeMap, ModelControls, ModelCostTable, ModelSettings, ProviderSettings,
    Result, RunLayer, ServerLayer, SettingsLayer, run,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveErrors(pub Vec<ResolveError>);

impl ResolveErrors {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, ResolveError> {
        self.0.iter()
    }

    #[must_use]
    pub fn into_inner(self) -> Vec<ResolveError> {
        self.0
    }
}

impl<'a> IntoIterator for &'a ResolveErrors {
    type Item = &'a ResolveError;
    type IntoIter = std::slice::Iter<'a, ResolveError>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl fmt::Display for ResolveErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered = self
            .0
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        f.write_str(&rendered)
    }
}

impl std::error::Error for ResolveErrors {}

impl From<Vec<ResolveError>> for ResolveErrors {
    fn from(value: Vec<ResolveError>) -> Self {
        Self(value)
    }
}

impl From<ResolveErrors> for Vec<ResolveError> {
    fn from(value: ResolveErrors) -> Self {
        value.0
    }
}

pub struct ServerSettingsBuilder;

impl ServerSettingsBuilder {
    pub fn load_default() -> Result<ServerSettings> {
        let layer = load_settings_config(None)?;
        Self::from_layer(&layer)
    }

    pub fn load_from(path: &Path) -> Result<ServerSettings> {
        let layer = load_settings_path(path, SettingsSource::ActiveSettings)?;
        Self::from_layer(&layer)
    }

    pub fn from_toml(source: &str) -> Result<ServerSettings> {
        let layer = parse_settings_toml(source, SettingsSource::ActiveSettings)?;
        Self::from_layer(&layer)
    }

    pub(crate) fn from_layer(layer: &SettingsLayer) -> Result<ServerSettings> {
        let layer = layer.clone().combine(DEFAULTS_LAYER.clone());
        let mut errors = Vec::new();
        let server = resolve_server(&layer.server.clone().unwrap_or_default(), &mut errors);
        finish_result(
            ServerSettings { server },
            "failed to resolve server settings",
            errors,
        )
    }
}

pub struct UserSettingsBuilder;

impl UserSettingsBuilder {
    pub fn load_default() -> Result<UserSettings> {
        let layer = load_settings_config(None)?;
        Self::from_layer(&layer)
    }

    pub fn load_default_with_cli_overrides(cli: &CliLayer) -> Result<UserSettings> {
        let layer = load_settings_config(None)?;
        Self::from_layer_with_cli_overrides(&layer, cli)
    }

    pub fn load_from(path: &Path) -> Result<UserSettings> {
        let layer = load_settings_path(path, SettingsSource::User)?;
        Self::from_layer(&layer)
    }

    pub fn load_from_with_cli_overrides(path: &Path, cli: &CliLayer) -> Result<UserSettings> {
        let layer = load_settings_path(path, SettingsSource::User)?;
        Self::from_layer_with_cli_overrides(&layer, cli)
    }

    pub fn from_toml(source: &str) -> Result<UserSettings> {
        let layer = parse_settings_toml(source, SettingsSource::User)?;
        Self::from_layer(&layer)
    }

    pub fn from_toml_with_cli_overrides(source: &str, cli: &CliLayer) -> Result<UserSettings> {
        let layer = parse_settings_toml(source, SettingsSource::User)?;
        Self::from_layer_with_cli_overrides(&layer, cli)
    }

    pub(crate) fn from_layer(layer: &SettingsLayer) -> Result<UserSettings> {
        let layer = layer.clone().combine(DEFAULTS_LAYER.clone());
        let mut errors = Vec::new();
        let cli = resolve_cli(&layer.cli.clone().unwrap_or_default(), &mut errors);
        finish_result(
            UserSettings { cli },
            "failed to resolve user settings",
            errors,
        )
    }

    pub(crate) fn from_layer_with_cli_overrides(
        layer: &SettingsLayer,
        cli: &CliLayer,
    ) -> Result<UserSettings> {
        Self::from_layer(
            &SettingsLayer {
                cli: Some(cli.clone()),
                ..SettingsLayer::default()
            }
            .combine(layer.clone()),
        )
    }
}

pub struct RunSettingsBuilder;

impl RunSettingsBuilder {
    pub fn load_default() -> Result<RunNamespace> {
        let layer = load_settings_config(None)?;
        Self::from_layer(&layer)
    }

    pub fn load_default_with_catalog(catalog: MergeMap<EnvironmentLayer>) -> Result<RunNamespace> {
        let mut layer = load_settings_config(None)?;
        layer.environments = layer.environments.combine(catalog);
        Self::from_layer(&layer)
    }

    pub fn load_from(path: &Path) -> Result<RunNamespace> {
        let layer = load_settings_path(path, SettingsSource::DirectRun)?;
        Self::from_layer(&layer)
    }

    pub fn load_from_with_catalog(
        path: &Path,
        catalog: MergeMap<EnvironmentLayer>,
    ) -> Result<RunNamespace> {
        let mut layer = load_settings_path(path, SettingsSource::DirectRun)?;
        layer.environments = layer.environments.combine(catalog);
        Self::from_layer(&layer)
    }

    pub fn from_toml(source: &str) -> Result<RunNamespace> {
        let layer = parse_settings_toml(source, SettingsSource::DirectRun)?;
        Self::from_layer(&layer)
    }

    pub fn from_toml_with_catalog(
        source: &str,
        catalog: MergeMap<EnvironmentLayer>,
    ) -> Result<RunNamespace> {
        let mut layer = parse_settings_toml(source, SettingsSource::DirectRun)?;
        layer.environments = layer.environments.combine(catalog);
        Self::from_layer(&layer)
    }

    pub(crate) fn from_layer(layer: &SettingsLayer) -> Result<RunNamespace> {
        let layer = layer.clone().combine(DEFAULTS_LAYER.clone());
        let mut errors = Vec::new();
        let run = resolve_run(
            &layer.run.clone().unwrap_or_default(),
            &layer.environments,
            &mut errors,
        );
        finish_result(run, "failed to resolve run settings", errors)
    }

    pub fn from_run_layer(run: &RunLayer) -> Result<RunNamespace> {
        Self::from_layer(&SettingsLayer {
            run: Some(run.clone()),
            ..SettingsLayer::default()
        })
    }
}

#[derive(Clone)]
pub struct ServerRuntimeSettings {
    pub server_settings:               ServerSettings,
    pub manifest_run_defaults:         RunLayer,
    pub manifest_environment_defaults: crate::MergeMap<crate::EnvironmentLayer>,
    pub manifest_run_settings:         std::result::Result<RunNamespace, SharedError>,
    pub llm_catalog_settings:          model_catalog::LlmCatalogSettings,
}

pub fn load_server_runtime_settings(
    path: Option<&Path>,
    run_overrides: Option<RunLayer>,
    server_overrides: Option<ServerLayer>,
) -> Result<ServerRuntimeSettings> {
    let layer = match path {
        Some(path) => load_settings_path(path, SettingsSource::ActiveSettings)?,
        None => load_settings_config(None)?,
    };
    resolve_server_runtime_settings(layer, run_overrides, server_overrides)
}

pub fn load_llm_catalog_settings(path: Option<&Path>) -> Result<model_catalog::LlmCatalogSettings> {
    let layer = match path {
        Some(path) => load_settings_path(path, SettingsSource::ActiveSettings)?,
        None => load_settings_config(None)?,
    };
    Ok(llm_catalog_settings_from_layer(&layer))
}

#[cfg(test)]
pub fn server_runtime_settings_from_toml(
    source: &str,
    run_overrides: Option<RunLayer>,
    server_overrides: Option<ServerLayer>,
) -> Result<ServerRuntimeSettings> {
    let layer = parse_settings_toml(source, SettingsSource::ActiveSettings)?;
    resolve_server_runtime_settings(layer, run_overrides, server_overrides)
}

fn resolve_server_runtime_settings(
    mut layer: SettingsLayer,
    run_overrides: Option<RunLayer>,
    server_overrides: Option<ServerLayer>,
) -> Result<ServerRuntimeSettings> {
    if let Some(run) = run_overrides {
        layer = SettingsLayer {
            run: Some(run),
            ..SettingsLayer::default()
        }
        .combine(layer);
    }
    if let Some(server) = server_overrides {
        layer = SettingsLayer {
            server: Some(server),
            ..SettingsLayer::default()
        }
        .combine(layer);
    }

    let manifest_run_defaults = layer.run.clone().unwrap_or_default();
    let manifest_environment_defaults = layer.environments.clone();
    let llm_catalog_settings = llm_catalog_settings_from_layer(&layer);
    Ok(ServerRuntimeSettings {
        server_settings: ServerSettingsBuilder::from_layer(&layer)?,
        manifest_run_settings: RunSettingsBuilder::from_layer(&SettingsLayer {
            run: Some(manifest_run_defaults.clone()),
            environments: manifest_environment_defaults.clone(),
            ..SettingsLayer::default()
        })
        .map_err(|err| SharedError::new(anyhow::Error::new(err))),
        manifest_run_defaults,
        manifest_environment_defaults,
        llm_catalog_settings,
    })
}

fn llm_catalog_settings_from_layer(layer: &SettingsLayer) -> model_catalog::LlmCatalogSettings {
    let layer = layer.clone().combine(DEFAULTS_LAYER.clone());
    layer
        .llm
        .map(llm_layer_to_catalog_settings)
        .unwrap_or_default()
}

fn llm_layer_to_catalog_settings(llm: LlmLayer) -> model_catalog::LlmCatalogSettings {
    model_catalog::LlmCatalogSettings {
        providers: llm
            .providers
            .into_inner()
            .into_iter()
            .map(|(id, settings)| (id, provider_settings_to_catalog(settings)))
            .collect(),
        models:    llm
            .models
            .into_inner()
            .into_iter()
            .map(|(id, settings)| (id, model_settings_to_catalog(settings)))
            .collect(),
    }
}

fn provider_settings_to_catalog(
    settings: ProviderSettings,
) -> model_catalog::ProviderCatalogSettings {
    model_catalog::ProviderCatalogSettings {
        display_name:   settings.display_name,
        adapter:        settings.adapter,
        agent_profile:  settings.agent_profile,
        auth:           settings.auth,
        billing_policy: settings.billing_policy,
        api_key_url:    settings.api_key_url,
        base_url:       settings.base_url,
        extra_headers:  settings.extra_headers,
        priority:       settings.priority,
        enabled:        settings.enabled,
        aliases:        settings.aliases,
    }
}

fn model_settings_to_catalog(settings: ModelSettings) -> model_catalog::ModelCatalogSettings {
    let ModelSettings {
        provider,
        api_id,
        agent_profile,
        display_name,
        family,
        training,
        knowledge_cutoff,
        default,
        small_default,
        probe,
        enabled,
        aliases,
        estimated_output_tps,
        limits,
        features,
        controls,
        costs,
    } = settings;
    model_catalog::ModelCatalogSettings {
        provider,
        api_id,
        agent_profile,
        display_name,
        family,
        training,
        knowledge_cutoff,
        default,
        small_default,
        probe,
        enabled,
        aliases,
        estimated_output_tps,
        limits: limits.as_ref().map(model_limits_to_catalog),
        features: features.as_ref().map(model_features_to_catalog),
        controls: controls.map(model_controls_to_catalog),
        costs: costs.as_ref().map(model_cost_table_to_catalog),
    }
}

fn model_limits_to_catalog(limits: &LlmModelLimits) -> model_catalog::SettingsModelLimits {
    model_catalog::SettingsModelLimits {
        context_window: limits.context_window,
        max_output:     limits.max_output,
    }
}

fn model_features_to_catalog(features: &LlmModelFeatures) -> model_catalog::SettingsModelFeatures {
    model_catalog::SettingsModelFeatures {
        tools:            features.tools,
        vision:           features.vision,
        reasoning:        features.reasoning,
        reasoning_effort: features.reasoning_effort,
        prompt_cache:     features.prompt_cache,
    }
}

fn model_controls_to_catalog(controls: ModelControls) -> model_catalog::SettingsModelControls {
    model_catalog::SettingsModelControls {
        reasoning_effort: controls.reasoning_effort,
        speed:            controls.speed,
    }
}

fn model_cost_table_to_catalog(costs: &ModelCostTable) -> model_catalog::SettingsModelCostTable {
    model_catalog::SettingsModelCostTable {
        base:  cost_rates_to_catalog(&costs.base),
        speed: costs.speed.as_ref().map(|speed| {
            speed
                .iter()
                .map(|(key, rates)| (key.clone(), cost_rates_to_catalog(rates)))
                .collect::<BTreeMap<_, _>>()
        }),
    }
}

fn cost_rates_to_catalog(rates: &CostRates) -> model_catalog::CostRates {
    model_catalog::CostRates {
        input_cost_per_mtok:       rates.input_cost_per_mtok,
        output_cost_per_mtok:      rates.output_cost_per_mtok,
        cache_input_cost_per_mtok: rates.cache_input_cost_per_mtok,
    }
}

fn parse_settings_toml(source: &str, kind: SettingsSource) -> Result<SettingsLayer> {
    let layer = source
        .parse::<SettingsLayer>()
        .map_err(|err| Error::parse("Failed to parse settings file", err))?;
    validate_settings_source(&layer, kind)
        .map_err(|err| Error::parse("Failed to parse settings file", err))?;
    Ok(layer)
}

#[derive(Clone, Debug, Default)]
pub struct WorkflowSettingsBuilder {
    args:     SettingsLayer,
    workflow: SettingsLayer,
    project:  SettingsLayer,
    user:     SettingsLayer,
    server:   SettingsLayer,
}

impl WorkflowSettingsBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_toml(source: &str) -> Result<WorkflowSettings> {
        let layer = parse_settings_toml(source, SettingsSource::Workflow)?;
        Self::from_layer(&layer)
            .map_err(|errors| Error::resolve("failed to resolve workflow settings", errors.into()))
    }

    #[must_use]
    pub(crate) fn args_layer(mut self, layer: SettingsLayer) -> Self {
        self.args = layer.combine(self.args);
        self
    }

    #[must_use]
    pub fn workflow_layer(mut self, layer: SettingsLayer) -> Self {
        self.workflow = layer;
        self
    }

    #[must_use]
    pub fn workflow_run_layer(self, run: RunLayer) -> Self {
        self.workflow_layer(SettingsLayer {
            run: Some(run),
            ..SettingsLayer::default()
        })
    }

    pub fn workflow_toml(self, source: &str) -> Result<Self> {
        let layer = parse_settings_toml(source, SettingsSource::Workflow)?;
        Ok(self.workflow_layer(layer))
    }

    pub fn workflow_toml_with_run_layer(self, source: &str, run: RunLayer) -> Result<Self> {
        let mut layer = parse_settings_toml(source, SettingsSource::Workflow)?;
        layer.run = Some(run);
        Ok(self.workflow_layer(layer))
    }

    pub fn workflow_file(self, path: &Path) -> Result<Self> {
        Ok(self.workflow_layer(run::load_run_config(path)?))
    }

    #[must_use]
    pub fn project_layer(mut self, layer: SettingsLayer) -> Self {
        self.project = layer;
        self
    }

    pub fn project_toml(self, source: &str) -> Result<Self> {
        let layer = parse_settings_toml(source, SettingsSource::Project)?;
        Ok(self.project_layer(layer))
    }

    pub fn project_toml_with_run_layer(self, source: &str, run: RunLayer) -> Result<Self> {
        let mut layer = parse_settings_toml(source, SettingsSource::Project)?;
        layer.run = Some(run);
        Ok(self.project_layer(layer))
    }

    pub fn project_file(self, path: &Path) -> Result<Self> {
        Ok(self.project_layer(load_settings_path(path, SettingsSource::Project)?))
    }

    #[must_use]
    pub(crate) fn user_layer(mut self, layer: SettingsLayer) -> Self {
        self.user = layer;
        self
    }

    pub fn user_toml(self, source: &str) -> Result<Self> {
        let layer = parse_settings_toml(source, SettingsSource::User)?;
        Ok(self.user_layer(layer))
    }

    pub fn user_file(self, path: &Path) -> Result<Self> {
        Ok(self.user_layer(load_settings_path(path, SettingsSource::User)?))
    }

    #[must_use]
    pub(crate) fn server_layer(mut self, layer: SettingsLayer) -> Self {
        self.server = layer;
        self
    }

    #[must_use]
    pub fn server_run_defaults(self, run: RunLayer) -> Self {
        self.server_layer(SettingsLayer {
            run: Some(run),
            ..SettingsLayer::default()
        })
    }

    #[must_use]
    pub fn server_manifest_defaults(
        self,
        run: RunLayer,
        environments: MergeMap<EnvironmentLayer>,
    ) -> Self {
        self.server_layer(SettingsLayer {
            run: Some(run),
            environments,
            ..SettingsLayer::default()
        })
    }

    #[must_use]
    pub fn run_overrides(self, run: RunLayer) -> Self {
        self.args_layer(SettingsLayer {
            run: Some(run),
            ..SettingsLayer::default()
        })
    }

    #[must_use]
    pub fn cli_overrides(self, cli: CliLayer) -> Self {
        self.args_layer(SettingsLayer {
            cli: Some(cli),
            ..SettingsLayer::default()
        })
    }

    #[must_use]
    pub(crate) fn build_layer(self) -> SettingsLayer {
        let server_defaults = SettingsLayer {
            version: self.server.version,
            run: self.server.run,
            environments: self.server.environments,
            ..SettingsLayer::default()
        };
        let mut layer = self
            .args
            .combine(self.workflow)
            .combine(self.project)
            .combine(self.user)
            .combine(server_defaults);
        layer = layer.combine(DEFAULTS_LAYER.clone());
        layer.server = None;
        layer.cli = None;
        layer
    }

    pub fn build(self) -> std::result::Result<WorkflowSettings, ResolveErrors> {
        Self::from_layer(&self.build_layer())
    }

    pub(crate) fn from_layer(
        layer: &SettingsLayer,
    ) -> std::result::Result<WorkflowSettings, ResolveErrors> {
        let layer = layer.clone().combine(DEFAULTS_LAYER.clone());
        let mut errors = Vec::new();
        let project = resolve_project(&layer.project.clone().unwrap_or_default(), &mut errors);
        let workflow = resolve_workflow(&layer.workflow.clone().unwrap_or_default(), &mut errors);
        let run = resolve_run(
            &layer.run.clone().unwrap_or_default(),
            &layer.environments,
            &mut errors,
        );
        finish_dense_result(
            WorkflowSettings {
                project,
                workflow,
                run,
            },
            errors,
        )
    }

    pub(crate) fn workflow_from_layer(
        layer: &SettingsLayer,
    ) -> std::result::Result<WorkflowNamespace, ResolveErrors> {
        let layer = layer.clone().combine(DEFAULTS_LAYER.clone());
        let mut errors = Vec::new();
        let workflow = resolve_workflow(&layer.workflow.clone().unwrap_or_default(), &mut errors);
        finish_dense_result(workflow, errors)
    }
}

fn finish_result<T>(value: T, context: &'static str, errors: Vec<ResolveError>) -> Result<T> {
    if errors.is_empty() {
        Ok(value)
    } else {
        Err(Error::resolve(context, errors))
    }
}

fn finish_dense_result<T>(
    value: T,
    errors: Vec<ResolveError>,
) -> std::result::Result<T, ResolveErrors> {
    if errors.is_empty() {
        Ok(value)
    } else {
        Err(errors.into())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_types::settings::InterpString;
    use fabro_types::settings::cli::OutputVerbosity;
    use fabro_types::settings::run::{ApprovalMode, EnvironmentProvider, RunMode};

    use super::{RunSettingsBuilder, WorkflowSettingsBuilder, server_runtime_settings_from_toml};
    use crate::{
        CliLayer, CliOutputLayer, ReplaceMap, RunExecutionLayer, RunLayer, RunModelLayer,
        SettingsLayer,
    };

    fn seeded_environment_catalog() -> crate::MergeMap<crate::EnvironmentLayer> {
        r#"
[environments.default]
provider = "local"
"#
        .parse::<SettingsLayer>()
        .expect("seeded catalog should parse")
        .environments
    }

    #[test]
    fn run_settings_builder_requires_injected_environment_catalog() {
        let err = RunSettingsBuilder::from_toml(
            r#"
_version = 1

[run.execution]
mode = "dry_run"

[run.agent.mcps.demo]
type = "stdio"
command = ["demo-mcp"]
"#,
        )
        .expect_err("run settings should not resolve without a server environment catalog");

        let message = err.to_string();
        assert!(
            message.contains("run.environment.id") && message.contains("unknown environment"),
            "expected missing server environment catalog diagnostic, got: {message}"
        );
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "test asserts the raw template source"
    )]
    #[test]
    fn workflow_builder_preserves_run_overrides_when_cli_overrides_are_added() {
        let settings = WorkflowSettingsBuilder::new()
            .server_manifest_defaults(RunLayer::default(), seeded_environment_catalog())
            .run_overrides(RunLayer {
                metadata: ReplaceMap::from(HashMap::from([("env".to_string(), "cli".to_string())])),
                model: Some(RunModelLayer {
                    provider:  Some(InterpString::parse("openai")),
                    name:      Some(InterpString::parse("gpt-5")),
                    fallbacks: Vec::new(),
                    controls:  None,
                }),
                execution: Some(RunExecutionLayer {
                    mode:     Some(RunMode::DryRun),
                    approval: Some(ApprovalMode::Auto),
                }),
                ..RunLayer::default()
            })
            .cli_overrides(CliLayer {
                output: Some(CliOutputLayer {
                    verbosity: Some(OutputVerbosity::Verbose),
                    ..CliOutputLayer::default()
                }),
                ..CliLayer::default()
            })
            .build()
            .expect("settings should resolve");

        assert_eq!(
            settings.run.metadata.get("env").map(String::as_str),
            Some("cli")
        );
        assert_eq!(
            settings
                .run
                .model
                .provider
                .as_ref()
                .map(InterpString::as_source),
            Some("openai".to_string())
        );
        assert_eq!(
            settings
                .run
                .model
                .name
                .as_ref()
                .map(InterpString::as_source),
            Some("gpt-5".to_string())
        );
        assert_eq!(settings.run.execution.mode, RunMode::DryRun);
        assert_eq!(settings.run.execution.approval, ApprovalMode::Auto);
    }

    #[test]
    fn workflow_environment_catalog_definition_overrides_server_catalog() {
        let settings = WorkflowSettingsBuilder::new()
            .server_manifest_defaults(RunLayer::default(), seeded_environment_catalog())
            .workflow_toml(
                r#"
_version = 1

[run.environment]
id = "default"

[environments.cloud]
provider = "docker"

[environments.default]
provider = "local"
"#,
            )
            .expect("workflow environment catalogs should be accepted")
            .build()
            .expect("settings should resolve");

        assert_eq!(
            settings.run.environment.provider,
            EnvironmentProvider::Local
        );
    }

    #[test]
    fn project_environment_catalog_definition_is_accepted() {
        let settings = WorkflowSettingsBuilder::new()
            .server_manifest_defaults(RunLayer::default(), seeded_environment_catalog())
            .project_toml(
                r#"
_version = 1

[run.environment]
id = "cloud"

[environments.cloud]
provider = "docker"
"#,
            )
            .expect("project environment catalogs should be accepted")
            .build()
            .expect("settings should resolve");

        assert_eq!(
            settings.run.environment.provider,
            EnvironmentProvider::Docker
        );
    }

    #[test]
    fn server_runtime_settings_preserves_llm_catalog_overrides() {
        let settings = server_runtime_settings_from_toml(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[llm.providers.acme]
display_name = "Acme"
adapter = "openai_compatible"
base_url = "https://api.acme.test/v1"
agent_profile = "anthropic"

[llm.providers.acme.auth]
credentials = ["env:ACME_API_KEY"]

[llm.models."acme-large"]
provider = "acme"
display_name = "Acme Large"
family = "acme"
default = true
agent_profile = "gemini"

[llm.models."acme-large".limits]
context_window = 128000

[llm.models."acme-large".features]
tools = true
vision = false
reasoning = false
"#,
            None,
            None,
        )
        .expect("server runtime settings should resolve");

        let catalog =
            fabro_model::Catalog::from_builtin_with_overrides(&settings.llm_catalog_settings)
                .expect("catalog overrides should build");

        assert_eq!(
            catalog
                .get("acme-large")
                .map(|model| model.provider.clone()),
            Some(fabro_model::ProviderId::new("acme"))
        );
        assert_eq!(
            catalog
                .effective_agent_profile(&fabro_model::ProviderId::new("acme"), Some("acme-large")),
            Some(fabro_model::AgentProfileKind::Gemini)
        );
    }
}
