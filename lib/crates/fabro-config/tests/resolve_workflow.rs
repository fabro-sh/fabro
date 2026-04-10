use fabro_config::resolve_workflow_from_file;
use fabro_types::settings::{SettingsFile, parse_settings_file};

#[test]
fn resolves_workflow_defaults_from_empty_settings() {
    let settings = SettingsFile::default();

    let workflow = resolve_workflow_from_file(&settings).expect("empty settings should resolve");

    assert_eq!(workflow.graph, "workflow.fabro");
    assert!(workflow.name.is_none());
    assert!(workflow.description.is_none());
    assert!(workflow.metadata.is_empty());
}

#[test]
fn resolves_workflow_graph_and_metadata() {
    let settings: SettingsFile = parse_settings_file(
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
    .expect("fixture should parse");

    let workflow = resolve_workflow_from_file(&settings).expect("workflow settings should resolve");

    assert_eq!(workflow.name.as_deref(), Some("Ship"));
    assert_eq!(workflow.description.as_deref(), Some("Primary flow"));
    assert_eq!(workflow.graph, "graphs/ship.dot");
    assert_eq!(
        workflow.metadata.get("tier").map(String::as_str),
        Some("gold")
    );
}
