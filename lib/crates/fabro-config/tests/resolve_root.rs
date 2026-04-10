use fabro_config::effective_settings::{EffectiveSettingsLayers, EffectiveSettingsMode};
use fabro_config::parse_settings_layer;
use fabro_types::settings::SettingsLayer;

fn parse(source: &str) -> SettingsLayer {
    parse_settings_layer(source).expect("fixture should parse")
}

#[test]
fn resolves_root_settings_defaults() {
    let settings =
        fabro_config::resolve(&SettingsLayer::default()).expect("empty settings should resolve");

    assert_eq!(settings.project.directory, "fabro/");
    assert_eq!(settings.workflow.graph, "workflow.fabro");
    assert_eq!(settings.run.execution.retros, true);
    assert_eq!(settings.cli.updates.check, true);
    assert_eq!(settings.server.scheduler.max_concurrent_runs, 5);
    assert_eq!(settings.features.session_sandboxes, false);
}

#[test]
fn resolve_accumulates_errors_across_namespaces() {
    let settings = parse(
        r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:3000"

[server.listen.tls]
cert = "/tmp/server.pem"

[server.auth.api.mtls]
enabled = true

[run.sandbox]
provider = "not-a-provider"
"#,
    );

    let errors = fabro_config::resolve(&settings).expect_err("invalid shape should fail");
    let rendered = errors
        .into_iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered.contains("server.listen.tls.key"));
    assert!(rendered.contains("server.listen.tls.ca"));
    assert!(rendered.contains("run.sandbox.provider"));
}

#[test]
fn load_and_resolve_merges_layers_before_resolution() {
    let settings = fabro_config::load_and_resolve(
        EffectiveSettingsLayers::new(
            SettingsLayer::default(),
            parse(
                r#"
_version = 1

[workflow]
graph = "graphs/workflow.dot"
"#,
            ),
            parse(
                r#"
_version = 1

[project]
directory = ".fabro"
"#,
            ),
            parse(
                r#"
_version = 1

[server.storage]
root = "/srv/fabro"

[run.model]
provider = "openai"
name = "gpt-5"
"#,
            ),
        ),
        None,
        EffectiveSettingsMode::LocalOnly,
    )
    .expect("layers should load and resolve");

    assert_eq!(settings.project.directory, ".fabro");
    assert_eq!(settings.workflow.graph, "graphs/workflow.dot");
    assert_eq!(settings.server.storage.root.as_source(), "/srv/fabro");
    assert_eq!(
        settings
            .run
            .model
            .provider
            .as_ref()
            .map(|value| value.as_source()),
        Some("openai".to_string())
    );
    assert_eq!(
        settings
            .run
            .model
            .name
            .as_ref()
            .map(|value| value.as_source()),
        Some("gpt-5".to_string())
    );
}
