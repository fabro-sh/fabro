use crate::{SettingsLayer, WorkflowSettingsBuilder};

#[test]
fn resolves_workflow_defaults_from_empty_settings() {
    let settings = SettingsLayer::default();

    let workflow = WorkflowSettingsBuilder::from_layer(&settings)
        .expect("empty settings should resolve")
        .workflow;

    assert_eq!(workflow.graph, "workflow.fabro");
    assert!(workflow.name.is_none());
    assert!(workflow.description.is_none());
    assert!(workflow.metadata.is_empty());
}

#[test]
fn resolves_workflow_graph_and_metadata() {
    let workflow = WorkflowSettingsBuilder::from_toml(
        r#"
_version = 1

[workflow]
name = "Ship"
description = "Primary flow"
graph = "graphs/ship.dot"

[workflow.metadata]
tier = "gold"
"#,
    )
    .expect("workflow settings should resolve")
    .workflow;

    assert_eq!(workflow.name.as_deref(), Some("Ship"));
    assert_eq!(workflow.description.as_deref(), Some("Primary flow"));
    assert_eq!(workflow.graph, "graphs/ship.dot");
    assert_eq!(
        workflow.metadata.get("tier").map(String::as_str),
        Some("gold")
    );
}
