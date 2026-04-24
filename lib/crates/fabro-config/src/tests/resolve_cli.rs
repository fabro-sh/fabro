#![expect(
    clippy::disallowed_methods,
    reason = "sync test fixture setup; not on a Tokio path"
)]

use fabro_types::settings::InterpString;
use fabro_types::settings::cli::{CliTargetSettings, OutputFormat, OutputVerbosity};
use fabro_types::settings::run::AgentPermissions;
use temp_env::with_var;

use crate::{SettingsLayer, UserSettingsBuilder};

#[test]
fn resolves_cli_defaults_from_empty_settings() {
    let settings = SettingsLayer::default();

    let cli = UserSettingsBuilder::from_layer(&settings)
        .expect("empty settings should resolve")
        .cli;

    assert!(cli.target.is_none());
    assert_eq!(cli.output.format, OutputFormat::Text);
    assert_eq!(cli.output.verbosity, OutputVerbosity::Normal);
    assert!(!cli.exec.prevent_idle_sleep);
    assert!(cli.updates.check);
    assert!(cli.logging.level.is_none());
}

#[test]
fn user_settings_from_layer_matches_namespace_resolvers() {
    let user_settings = fabro_config::UserSettingsBuilder::from_toml(
        r#"
_version = 1

[cli.target]
type = "http"
url = "https://config.example.com"

[features]
session_sandboxes = true
"#,
    )
    .expect("user settings should resolve");

    assert_eq!(
        user_settings.cli.target,
        Some(CliTargetSettings::Http {
            url: InterpString::parse("https://config.example.com"),
        })
    );
    assert!(user_settings.features.session_sandboxes);
}

#[test]
fn user_settings_resolve_reads_default_settings_from_fabro_home() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("settings.toml"),
        r#"
_version = 1

[cli.output]
verbosity = "verbose"

[features]
session_sandboxes = true
"#,
    )
    .unwrap();

    with_var("FABRO_HOME", Some(home.path()), || {
        let user_settings = fabro_config::UserSettingsBuilder::load_default()
            .expect("user settings should resolve");
        assert_eq!(user_settings.cli.output.verbosity, OutputVerbosity::Verbose);
        assert!(user_settings.features.session_sandboxes);
    });
}

#[test]
fn user_settings_resolve_returns_defaults_when_default_settings_file_is_missing() {
    let home = tempfile::tempdir().unwrap();

    with_var("FABRO_HOME", Some(home.path()), || {
        let user_settings = fabro_config::UserSettingsBuilder::load_default()
            .expect("user settings should resolve");
        assert_eq!(user_settings.cli.output.format, OutputFormat::Text);
        assert_eq!(user_settings.cli.output.verbosity, OutputVerbosity::Normal);
        assert!(!user_settings.features.session_sandboxes);
    });
}

#[test]
fn resolves_cli_target_exec_and_output_settings() {
    let cli = UserSettingsBuilder::from_toml(
        r#"
_version = 1

[cli.target]
type = "http"
url = "https://config.example.com"

[cli.exec]
prevent_idle_sleep = true

[cli.exec.model]
provider = "openai"
name = "gpt-5"

[cli.exec.agent]
permissions = "read-only"

[cli.exec.agent.mcps.fs]
type = "stdio"
command = ["echo", "cli"]

[cli.output]
format = "json"
verbosity = "verbose"

[cli.updates]
check = false

[cli.logging]
level = "debug"
"#,
    )
    .expect("cli settings should resolve")
    .cli;

    let CliTargetSettings::Http { url } = cli.target.expect("target") else {
        panic!("expected http target");
    };
    assert_eq!(url.as_source(), "https://config.example.com");

    assert!(cli.exec.prevent_idle_sleep);
    assert_eq!(
        cli.exec
            .model
            .provider
            .as_ref()
            .map(InterpString::as_source),
        Some("openai".to_string())
    );
    assert_eq!(
        cli.exec.model.name.as_ref().map(InterpString::as_source),
        Some("gpt-5".to_string())
    );
    assert_eq!(cli.exec.agent.permissions, Some(AgentPermissions::ReadOnly));
    assert_eq!(cli.exec.agent.mcps["fs"].name, "fs");
    assert_eq!(cli.output.format, OutputFormat::Json);
    assert_eq!(cli.output.verbosity, OutputVerbosity::Verbose);
    assert!(!cli.updates.check);
    assert_eq!(cli.logging.level.as_deref(), Some("debug"));
}
