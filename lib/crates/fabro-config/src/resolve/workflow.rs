use fabro_types::settings::workflow::{WorkflowLayer, WorkflowSettings};

use super::ResolveError;

const DEFAULT_WORKFLOW_GRAPH: &str = "workflow.fabro";

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
            .unwrap_or_else(|| DEFAULT_WORKFLOW_GRAPH.to_string()),
        metadata: layer.metadata.clone(),
    }
}
