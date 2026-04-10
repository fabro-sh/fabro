use super::{
    CliSettings, FeaturesSettings, ProjectSettings, RunSettings, ServerSettings, WorkflowSettings,
};

/// A fully resolved settings view across all namespaces.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settings {
    pub project: ProjectSettings,
    pub workflow: WorkflowSettings,
    pub run: RunSettings,
    pub cli: CliSettings,
    pub server: ServerSettings,
    pub features: FeaturesSettings,
}
