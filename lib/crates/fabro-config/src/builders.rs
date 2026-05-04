use std::fmt;
use std::path::Path;

use fabro_types::settings::{ProjectNamespace, RunNamespace, WorkflowNamespace};
use fabro_types::{ServerSettings, UserSettings, WorkflowSettings};
use fabro_util::error::SharedError;

use crate::defaults::DEFAULTS_LAYER;
use crate::load::load_settings_path;
use crate::resolve::{
    ResolveError, resolve_cli, resolve_features, resolve_project, resolve_run, resolve_server,
    resolve_workflow,
};
use crate::user::load_settings_config;
use crate::{CliLayer, Combine, Error, Result, RunLayer, ServerLayer, SettingsLayer, run};

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
        let layer = load_settings_path(path)?;
        Self::from_layer(&layer)
    }

    pub fn from_toml(source: &str) -> Result<ServerSettings> {
        let layer = source
            .parse::<SettingsLayer>()
            .map_err(|err| Error::parse("Failed to parse settings file", err))?;
        Self::from_layer(&layer)
    }

    pub(crate) fn from_layer(layer: &SettingsLayer) -> Result<ServerSettings> {
        let layer = layer.clone().combine(DEFAULTS_LAYER.clone());
        let mut errors = Vec::new();
        let server = resolve_server(&layer.server.clone().unwrap_or_default(), &mut errors);
        let features = resolve_features(&layer.features.clone().unwrap_or_default(), &mut errors);
        finish_result(
            ServerSettings { server, features },
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
        let layer = load_settings_path(path)?;
        Self::from_layer(&layer)
    }

    pub fn load_from_with_cli_overrides(path: &Path, cli: &CliLayer) -> Result<UserSettings> {
        let layer = load_settings_path(path)?;
        Self::from_layer_with_cli_overrides(&layer, cli)
    }

    pub fn from_toml(source: &str) -> Result<UserSettings> {
        let layer = source
            .parse::<SettingsLayer>()
            .map_err(|err| Error::parse("Failed to parse settings file", err))?;
        Self::from_layer(&layer)
    }

    pub fn from_toml_with_cli_overrides(source: &str, cli: &CliLayer) -> Result<UserSettings> {
        let layer = source
            .parse::<SettingsLayer>()
            .map_err(|err| Error::parse("Failed to parse settings file", err))?;
        Self::from_layer_with_cli_overrides(&layer, cli)
    }

    pub(crate) fn from_layer(layer: &SettingsLayer) -> Result<UserSettings> {
        let layer = layer.clone().combine(DEFAULTS_LAYER.clone());
        let mut errors = Vec::new();
        let cli = resolve_cli(&layer.cli.clone().unwrap_or_default(), &mut errors);
        let features = resolve_features(&layer.features.clone().unwrap_or_default(), &mut errors);
        finish_result(
            UserSettings { cli, features },
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

    pub fn load_from(path: &Path) -> Result<RunNamespace> {
        let layer = load_settings_path(path)?;
        Self::from_layer(&layer)
    }

    pub fn from_toml(source: &str) -> Result<RunNamespace> {
        let layer = source
            .parse::<SettingsLayer>()
            .map_err(|err| Error::parse("Failed to parse settings file", err))?;
        Self::from_layer(&layer)
    }

    pub(crate) fn from_layer(layer: &SettingsLayer) -> Result<RunNamespace> {
        let layer = layer.clone().combine(DEFAULTS_LAYER.clone());
        let mut errors = Vec::new();
        let run = resolve_run(&layer.run.clone().unwrap_or_default(), &mut errors);
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
    pub server_settings:       ServerSettings,
    pub manifest_run_defaults: RunLayer,
    pub manifest_run_settings: std::result::Result<RunNamespace, SharedError>,
}

pub fn load_server_runtime_settings(
    path: Option<&Path>,
    run_overrides: Option<RunLayer>,
    server_overrides: Option<ServerLayer>,
) -> Result<ServerRuntimeSettings> {
    let layer = match path {
        Some(path) => load_settings_path(path)?,
        None => load_settings_config(None)?,
    };
    resolve_server_runtime_settings(layer, run_overrides, server_overrides)
}

#[cfg(test)]
pub fn server_runtime_settings_from_toml(
    source: &str,
    run_overrides: Option<RunLayer>,
    server_overrides: Option<ServerLayer>,
) -> Result<ServerRuntimeSettings> {
    let layer = source
        .parse::<SettingsLayer>()
        .map_err(|err| Error::parse("Failed to parse settings file", err))?;
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
    Ok(ServerRuntimeSettings {
        server_settings: ServerSettingsBuilder::from_layer(&layer)?,
        manifest_run_settings: RunSettingsBuilder::from_run_layer(&manifest_run_defaults)
            .map_err(|err| SharedError::new(anyhow::Error::new(err))),
        manifest_run_defaults,
    })
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
        let layer = source
            .parse::<SettingsLayer>()
            .map_err(|err| Error::parse("Failed to parse settings file", err))?;
        Self::from_layer(&layer)
            .map_err(|errors| Error::resolve("failed to resolve workflow settings", errors.into()))
    }

    #[must_use]
    pub(crate) fn args_layer(mut self, layer: SettingsLayer) -> Self {
        self.args = layer.combine(self.args);
        self
    }

    #[must_use]
    pub(crate) fn workflow_layer(mut self, layer: SettingsLayer) -> Self {
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
        let layer = source
            .parse::<SettingsLayer>()
            .map_err(|err| Error::parse("Failed to parse settings file", err))?;
        Ok(self.workflow_layer(layer))
    }

    pub fn workflow_file(self, path: &Path) -> Result<Self> {
        Ok(self.workflow_layer(run::load_run_config(path)?))
    }

    #[must_use]
    pub(crate) fn project_layer(mut self, layer: SettingsLayer) -> Self {
        self.project = layer;
        self
    }

    pub fn project_toml(self, source: &str) -> Result<Self> {
        let layer = source
            .parse::<SettingsLayer>()
            .map_err(|err| Error::parse("Failed to parse settings file", err))?;
        Ok(self.project_layer(layer))
    }

    pub fn project_file(self, path: &Path) -> Result<Self> {
        Ok(self.project_layer(load_settings_path(path)?))
    }

    #[must_use]
    pub(crate) fn user_layer(mut self, layer: SettingsLayer) -> Self {
        self.user = layer;
        self
    }

    pub fn user_toml(self, source: &str) -> Result<Self> {
        let layer = source
            .parse::<SettingsLayer>()
            .map_err(|err| Error::parse("Failed to parse settings file", err))?;
        Ok(self.user_layer(layer))
    }

    pub fn user_file(self, path: &Path) -> Result<Self> {
        Ok(self.user_layer(load_settings_path(path)?))
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
        layer.features = None;
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
        let run = resolve_run(&layer.run.clone().unwrap_or_default(), &mut errors);
        finish_dense_result(
            WorkflowSettings {
                project,
                workflow,
                run,
            },
            errors,
        )
    }

    pub(crate) fn project_from_layer(
        layer: &SettingsLayer,
    ) -> std::result::Result<ProjectNamespace, ResolveErrors> {
        let layer = layer.clone().combine(DEFAULTS_LAYER.clone());
        let mut errors = Vec::new();
        let project = resolve_project(&layer.project.clone().unwrap_or_default(), &mut errors);
        finish_dense_result(project, errors)
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
    use fabro_types::settings::run::{ApprovalMode, RunMode};

    use super::{RunSettingsBuilder, WorkflowSettingsBuilder};
    use crate::{CliLayer, CliOutputLayer, ReplaceMap, RunExecutionLayer, RunLayer, RunModelLayer};

    #[test]
    fn run_settings_builder_resolves_run_namespace() {
        let settings = RunSettingsBuilder::from_toml(
            r#"
_version = 1

[run.execution]
mode = "dry_run"

[run.agent.mcps.demo]
type = "stdio"
command = ["demo-mcp"]
"#,
        )
        .expect("run settings should resolve");

        assert_eq!(settings.execution.mode, RunMode::DryRun);
        assert!(settings.agent.mcps.contains_key("demo"));
    }

    #[test]
    fn workflow_builder_preserves_run_overrides_when_cli_overrides_are_added() {
        let settings = WorkflowSettingsBuilder::new()
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
                    retros:   Some(false),
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
        assert!(!settings.run.execution.retros);
    }
}
