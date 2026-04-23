use fabro_types::settings::cli::OutputFormat;
use fabro_types::settings::run::{ApprovalMode, RunMode, WorktreeMode};
use fabro_types::settings::server::ObjectStoreProvider;

use crate::{Combine, ServerSettingsBuilder, SettingsLayer, WorkflowSettingsBuilder};

fn parse(source: &str) -> SettingsLayer {
    source.parse::<SettingsLayer>().expect("fixture should parse")
}

fn embedded_defaults() -> SettingsLayer {
    parse(include_str!("../defaults.toml"))
}

#[test]
fn embedded_defaults_parse_successfully() {
    let defaults = embedded_defaults();

    assert_eq!(
        defaults
            .project
            .as_ref()
            .and_then(|project| project.directory.as_deref()),
        Some(".")
    );
    assert_eq!(
        defaults
            .workflow
            .as_ref()
            .and_then(|workflow| workflow.graph.as_deref()),
        Some("workflow.fabro")
    );
}

#[test]
fn apply_builtin_defaults_materializes_expected_layer() {
    let layer = SettingsLayer::default().combine(embedded_defaults());

    assert_eq!(
        layer
            .project
            .as_ref()
            .and_then(|project| project.directory.as_deref()),
        Some(".")
    );
    assert_eq!(
        layer
            .workflow
            .as_ref()
            .and_then(|workflow| workflow.graph.as_deref()),
        Some("workflow.fabro")
    );
    assert_eq!(
        layer
            .run
            .as_ref()
            .and_then(|run| run.execution.as_ref())
            .and_then(|execution| execution.mode),
        Some(RunMode::Normal)
    );
    assert_eq!(
        layer
            .run
            .as_ref()
            .and_then(|run| run.execution.as_ref())
            .and_then(|execution| execution.approval),
        Some(ApprovalMode::Prompt)
    );
    assert_eq!(
        layer
            .run
            .as_ref()
            .and_then(|run| run.sandbox.as_ref())
            .and_then(|sandbox| sandbox.local.as_ref())
            .and_then(|local| local.worktree_mode),
        Some(WorktreeMode::Clean)
    );
    assert_eq!(
        layer
            .cli
            .as_ref()
            .and_then(|cli| cli.output.as_ref())
            .and_then(|output| output.format),
        Some(OutputFormat::Text)
    );
    assert_eq!(
        layer
            .server
            .as_ref()
            .and_then(|server| server.artifacts.as_ref())
            .and_then(|artifacts| artifacts.provider),
        Some(ObjectStoreProvider::Local)
    );
}

#[test]
fn resolve_empty_settings_requires_explicit_server_auth_methods() {
    let errors = ServerSettingsBuilder::from_layer(&SettingsLayer::default())
        .expect_err("empty server settings should fail");

    assert!(matches!(
        errors,
        fabro_config::Error::Resolve { errors, .. }
            if errors.iter().any(|error| {
                matches!(
                    error,
                    fabro_config::ResolveError::Missing { path } if path == "server.auth.methods"
                )
            })
    ));
}

#[test]
fn higher_precedence_values_override_builtin_defaults() {
    let layer = parse(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[run.execution]
mode = "dry_run"
"#,
    );

    let settings =
        WorkflowSettingsBuilder::from_layer(&layer).expect("workflow settings should resolve");

    assert_eq!(settings.run.execution.mode, RunMode::DryRun);
    assert_eq!(settings.run.execution.approval, ApprovalMode::Prompt);
    assert_eq!(settings.workflow.graph, "workflow.fabro");
}
