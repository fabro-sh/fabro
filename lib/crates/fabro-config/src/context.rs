use std::collections::HashMap;

use fabro_types::settings::{
    CliNamespace, FeaturesNamespace, ProjectNamespace, RunNamespace, ServerNamespace,
    SettingsLayer, WorkflowNamespace,
};
use serde::{Deserialize, Serialize};

use crate::user::load_settings_config;
use crate::{
    Error, ResolveError, Result, apply_builtin_defaults, resolve_cli, resolve_features,
    resolve_project, resolve_run, resolve_server, resolve_workflow,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerSettings {
    pub server:   ServerNamespace,
    pub features: FeaturesNamespace,
}

impl ServerSettings {
    pub fn from_layer(layer: &SettingsLayer) -> Result<Self> {
        let layer = apply_builtin_defaults(layer.clone());
        let mut errors = Vec::new();
        let server = resolve_server(&layer.server.clone().unwrap_or_default(), &mut errors);
        let features = resolve_features(&layer.features.clone().unwrap_or_default(), &mut errors);
        if errors.is_empty() {
            Ok(Self { server, features })
        } else {
            Err(Error::resolve("failed to resolve server settings", errors))
        }
    }

    pub fn resolve() -> Result<Self> {
        let layer = load_settings_config(None)?;
        Self::from_layer(&layer)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UserSettings {
    pub cli:      CliNamespace,
    pub features: FeaturesNamespace,
}

impl UserSettings {
    pub fn from_layer(layer: &SettingsLayer) -> Result<Self> {
        let layer = apply_builtin_defaults(layer.clone());
        let mut errors = Vec::new();
        let cli = resolve_cli(&layer.cli.clone().unwrap_or_default(), &mut errors);
        let features = resolve_features(&layer.features.clone().unwrap_or_default(), &mut errors);
        if errors.is_empty() {
            Ok(Self { cli, features })
        } else {
            Err(Error::resolve("failed to resolve user settings", errors))
        }
    }

    pub fn resolve() -> Result<Self> {
        let layer = load_settings_config(None)?;
        Self::from_layer(&layer)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkflowSettings {
    pub project:  ProjectNamespace,
    pub workflow: WorkflowNamespace,
    pub run:      RunNamespace,
}

impl WorkflowSettings {
    pub fn from_layer(layer: &SettingsLayer) -> std::result::Result<Self, Vec<ResolveError>> {
        let layer = apply_builtin_defaults(layer.clone());
        let mut errors = Vec::new();
        let project = resolve_project(&layer.project.clone().unwrap_or_default(), &mut errors);
        let workflow = resolve_workflow(&layer.workflow.clone().unwrap_or_default(), &mut errors);
        let run = resolve_run(&layer.run.clone().unwrap_or_default(), &mut errors);
        if errors.is_empty() {
            Ok(Self {
                project,
                workflow,
                run,
            })
        } else {
            Err(errors)
        }
    }

    pub fn combined_labels(&self) -> HashMap<String, String> {
        let mut labels = self.project.metadata.clone();
        labels.extend(self.workflow.metadata.clone());
        labels.extend(self.run.metadata.clone());
        labels
    }
}
