use fabro_types::settings::project::{ProjectLayer, ProjectSettings};

use super::ResolveError;

pub fn resolve_project(layer: &ProjectLayer, _errors: &mut Vec<ResolveError>) -> ProjectSettings {
    ProjectSettings {
        name: layer.name.clone(),
        description: layer.description.clone(),
        directory: layer
            .directory
            .clone()
            .expect("defaults.toml should provide project.directory"),
        metadata: layer.metadata.clone(),
    }
}
