use fabro_types::settings::project::{ProjectLayer, ProjectSettings};

use super::ResolveError;

const DEFAULT_PROJECT_DIRECTORY: &str = "fabro/";

pub fn resolve_project(layer: &ProjectLayer, _errors: &mut Vec<ResolveError>) -> ProjectSettings {
    ProjectSettings {
        name: layer.name.clone(),
        description: layer.description.clone(),
        directory: layer
            .directory
            .clone()
            .unwrap_or_else(|| DEFAULT_PROJECT_DIRECTORY.to_string()),
        metadata: layer.metadata.clone(),
    }
}
