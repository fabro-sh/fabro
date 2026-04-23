use fabro_config::{WorkflowSettingsBuilder, parse_settings_layer};
use fabro_types::settings::SettingsLayer;

#[test]
fn resolves_project_defaults_from_empty_settings() {
    let settings = SettingsLayer::default();

    let project = WorkflowSettingsBuilder::from_layer(&settings)
        .expect("empty settings should resolve")
        .project;

    assert_eq!(project.directory, ".");
    assert!(project.name.is_none());
    assert!(project.description.is_none());
    assert!(project.metadata.is_empty());
}

#[test]
fn resolves_project_directory_and_metadata() {
    let settings: SettingsLayer = parse_settings_layer(
        r#"
_version = 1

[project]
name = "Acme"
description = "Automation"
directory = ".fabro"

[project.metadata]
team = "platform"
"#,
    )
    .expect("fixture should parse");

    let project = WorkflowSettingsBuilder::from_layer(&settings)
        .expect("project settings should resolve")
        .project;

    assert_eq!(project.name.as_deref(), Some("Acme"));
    assert_eq!(project.description.as_deref(), Some("Automation"));
    assert_eq!(project.directory, ".fabro");
    assert_eq!(
        project.metadata.get("team").map(String::as_str),
        Some("platform")
    );
}
