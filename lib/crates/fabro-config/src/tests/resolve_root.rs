use fabro_types::settings::InterpString;
use fabro_types::settings::run::RunMode;

use crate::{ServerSettingsBuilder, SettingsLayer, WorkflowSettingsBuilder};

#[test]
fn resolves_root_settings_require_explicit_server_auth_methods() {
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
fn resolve_accumulates_errors_across_namespaces() {
    let source = r#"
_version = 1

[server.listen]
type = "tcp"
address = "not-a-socket-addr"

[server.auth]
methods = ["github"]

[server.auth.github]
allowed_usernames = []

[run.sandbox]
provider = "not-a-provider"
"#;

    let mut rendered = Vec::new();
    rendered.extend(
        match ServerSettingsBuilder::from_toml(source)
            .expect_err("invalid server settings should fail")
        {
            fabro_config::Error::Resolve { errors, .. } => errors,
            other => panic!("expected resolve error, got {other:#}"),
        }
        .into_iter()
        .map(|error| error.to_string()),
    );
    rendered.extend(
        match fabro_config::WorkflowSettingsBuilder::from_toml(source)
            .expect_err("invalid run settings should fail")
        {
            fabro_config::Error::Resolve { errors, .. } => errors,
            other => panic!("expected resolve error, got {other:#}"),
        }
        .into_iter()
        .map(|error| error.to_string()),
    );
    let rendered = rendered.join("\n");

    assert!(rendered.contains("server.listen.address"));
    assert!(rendered.contains("server.auth.github.allowed_usernames"));
    assert!(rendered.contains("run.sandbox.provider"));
}

#[test]
fn namespace_resolvers_cover_root_level_settings_shape() {
    let source = r#"
_version = 1

[project]
directory = ".fabro"

[workflow]
graph = "graphs/workflow.dot"

[server.storage]
root = "/srv/fabro"

[server.auth]
methods = ["dev-token"]
[run.model]
provider = "openai"
name = "gpt-5"
"#;

    let workflow_settings =
        WorkflowSettingsBuilder::from_toml(source).expect("workflow settings should resolve");
    let server = ServerSettingsBuilder::from_toml(source).expect("server settings should resolve");

    assert_eq!(workflow_settings.project.directory, ".fabro");
    assert_eq!(workflow_settings.workflow.graph, "graphs/workflow.dot");
    assert_eq!(server.server.storage.root.as_source(), "/srv/fabro");
    assert_eq!(
        workflow_settings
            .run
            .model
            .provider
            .as_ref()
            .map(InterpString::as_source),
        Some("openai".to_string())
    );
    assert_eq!(
        workflow_settings
            .run
            .model
            .name
            .as_ref()
            .map(InterpString::as_source),
        Some("gpt-5".to_string())
    );
}

#[test]
fn workflow_settings_resolve_defaults_and_expose_fields() {
    let settings = SettingsLayer::default();
    let resolved = fabro_config::WorkflowSettingsBuilder::from_layer(&settings)
        .expect("defaults should resolve");

    assert_eq!(resolved.project.directory, ".");
    assert_eq!(resolved.workflow.graph, "workflow.fabro");
    assert_eq!(resolved.run.execution.mode, RunMode::Normal);
}

#[test]
fn workflow_settings_combine_labels_with_later_namespaces_winning() {
    let labels = fabro_config::WorkflowSettingsBuilder::from_toml(
        r#"
_version = 1

[project.metadata]
project = "yes"
shared = "project"

[workflow.metadata]
workflow = "yes"
shared = "workflow"

[run.metadata]
run = "yes"
shared = "run"
"#,
    )
    .expect("workflow settings should resolve")
    .combined_labels();

    assert_eq!(labels.get("project").map(String::as_str), Some("yes"));
    assert_eq!(labels.get("workflow").map(String::as_str), Some("yes"));
    assert_eq!(labels.get("run").map(String::as_str), Some("yes"));
    assert_eq!(labels.get("shared").map(String::as_str), Some("run"));
}

#[test]
fn workflow_settings_report_invalid_run_sandbox_provider() {
    let errors = match fabro_config::WorkflowSettingsBuilder::from_toml(
        r#"
_version = 1

[run.sandbox]
provider = "not-a-provider"
"#,
    )
    .expect_err("invalid workflow settings should fail")
    {
        fabro_config::Error::Resolve { errors, .. } => errors,
        other => panic!("expected resolve error, got {other:#}"),
    };

    assert!(errors.iter().any(|error| {
        matches!(
            error,
            fabro_config::ResolveError::Invalid { path, .. } if path == "run.sandbox.provider"
        )
    }));
}

#[test]
fn workflow_settings_accumulate_multiple_run_errors() {
    let rendered = fabro_config::WorkflowSettingsBuilder::from_toml(
        r#"
_version = 1

[run.sandbox]
provider = "not-a-provider"

[[run.prepare.steps]]
script = "echo hi"
command = ["echo", "hi"]
"#,
    )
    .expect_err("invalid workflow settings should fail")
    .to_string();

    assert!(rendered.contains("run.sandbox.provider"));
    assert!(rendered.contains("run.prepare.steps[0]"));
}
