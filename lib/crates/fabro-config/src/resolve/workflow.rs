use fabro_types::settings::workflow::{WorkflowLayer, WorkflowSettings};

use super::ResolveError;

pub fn resolve_workflow(
    layer: &WorkflowLayer,
    _errors: &mut Vec<ResolveError>,
) -> WorkflowSettings {
    WorkflowSettings {
        name: layer.name.clone(),
        description: layer.description.clone(),
        graph: layer
            .graph
            .clone()
            .expect("defaults.toml should provide workflow.graph"),
        metadata: layer.metadata.clone(),
    }
}
