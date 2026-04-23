use fabro_types::settings::project::{ProjectLayer, ProjectNamespace};

use super::ResolveError;

pub fn resolve_project(layer: &ProjectLayer, _errors: &mut Vec<ResolveError>) -> ProjectNamespace {
    ProjectNamespace {
        name:        layer.name.clone(),
        description: layer.description.clone(),
        directory:   layer
            .directory
            .clone()
            .expect("defaults.toml should provide project.directory"),
        metadata:    layer.metadata.clone().into_inner(),
    }
}
