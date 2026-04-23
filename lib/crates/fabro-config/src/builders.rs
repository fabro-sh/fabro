use std::fmt;
use std::path::Path;

use fabro_types::settings::{
    CliLayer, Combine, ProjectNamespace, RunLayer, RunNamespace, SettingsLayer, WorkflowNamespace,
};
use fabro_types::{ServerSettings, UserSettings, WorkflowSettings};

use crate::defaults::DEFAULTS_LAYER;
use crate::load::load_settings_path;
use crate::parse::parse_settings_layer;
use crate::resolve::{
    ResolveError, resolve_cli, resolve_features, resolve_project, resolve_run, resolve_server,
    resolve_workflow,
};
use crate::user::load_settings_config;
use crate::{Error, Result, run};

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
        let layer = parse_settings_layer(source)
            .map_err(|err| Error::parse("Failed to parse settings file", err))?;
        Self::from_layer(&layer)
    }

    pub fn from_layer(layer: &SettingsLayer) -> Result<ServerSettings> {
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

    pub fn load_from(path: &Path) -> Result<UserSettings> {
        let layer = load_settings_path(path)?;
        Self::from_layer(&layer)
    }

    pub fn from_toml(source: &str) -> Result<UserSettings> {
        let layer = parse_settings_layer(source)
            .map_err(|err| Error::parse("Failed to parse settings file", err))?;
        Self::from_layer(&layer)
    }

    pub fn from_layer(layer: &SettingsLayer) -> Result<UserSettings> {
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
        let layer = parse_settings_layer(source)
            .map_err(|err| Error::parse("Failed to parse settings file", err))?;
        Self::from_layer(&layer)
    }

    pub fn from_layer(layer: &SettingsLayer) -> Result<RunNamespace> {
        let layer = layer.clone().combine(DEFAULTS_LAYER.clone());
        let mut errors = Vec::new();
        let run = resolve_run(&layer.run.clone().unwrap_or_default(), &mut errors);
        finish_result(run, "failed to resolve run settings", errors)
    }
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
        let layer = parse_settings_layer(source)
            .map_err(|err| Error::parse("Failed to parse settings file", err))?;
        Self::from_layer(&layer)
            .map_err(|errors| Error::resolve("failed to resolve workflow settings", errors.into()))
    }

    #[must_use]
    pub fn args_layer(mut self, layer: SettingsLayer) -> Self {
        self.args = layer;
        self
    }

    #[must_use]
    pub fn workflow_layer(mut self, layer: SettingsLayer) -> Self {
        self.workflow = layer;
        self
    }

    pub fn workflow_file(self, path: &Path) -> Result<Self> {
        Ok(self.workflow_layer(run::load_run_config(path)?))
    }

    #[must_use]
    pub fn project_layer(mut self, layer: SettingsLayer) -> Self {
        self.project = layer;
        self
    }

    pub fn project_file(self, path: &Path) -> Result<Self> {
        Ok(self.project_layer(load_settings_path(path)?))
    }

    #[must_use]
    pub fn user_layer(mut self, layer: SettingsLayer) -> Self {
        self.user = layer;
        self
    }

    pub fn user_file(self, path: &Path) -> Result<Self> {
        Ok(self.user_layer(load_settings_path(path)?))
    }

    #[must_use]
    pub fn server_layer(mut self, layer: SettingsLayer) -> Self {
        self.server = layer;
        self
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
    pub fn build_layer(self) -> SettingsLayer {
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

    pub fn from_layer(
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

    pub(crate) fn run_from_layer(
        layer: &SettingsLayer,
    ) -> std::result::Result<RunNamespace, ResolveErrors> {
        let layer = layer.clone().combine(DEFAULTS_LAYER.clone());
        let mut errors = Vec::new();
        let run = resolve_run(&layer.run.clone().unwrap_or_default(), &mut errors);
        finish_dense_result(run, errors)
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
    use fabro_types::settings::run::RunMode;

    use super::RunSettingsBuilder;

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
}
