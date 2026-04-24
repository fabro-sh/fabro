use fabro_types::settings::WorkflowNamespace;

use super::ResolveError;
use crate::WorkflowLayer;

pub fn resolve_workflow(
    layer: &WorkflowLayer,
    _errors: &mut Vec<ResolveError>,
) -> WorkflowNamespace {
    WorkflowNamespace {
        name:        layer.name.clone(),
        description: layer.description.clone(),
        graph:       layer
            .graph
            .clone()
            .expect("defaults.toml should provide workflow.graph"),
        metadata:    layer.metadata.clone().into_inner(),
    }
}
