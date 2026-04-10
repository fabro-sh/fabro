use fabro_config::parse_settings_layer;
use fabro_config::resolve_cli_from_file;
use fabro_types::settings::cli::{CliTargetSettings, OutputFormat, OutputVerbosity};
use fabro_types::settings::run::AgentPermissions;
use fabro_types::settings::{InterpString, SettingsLayer};

#[test]
fn resolves_cli_defaults_from_empty_settings() {
    let settings = SettingsLayer::default();

    let cli = resolve_cli_from_file(&settings).expect("empty settings should resolve");

    assert!(cli.target.is_none());
    assert_eq!(cli.output.format, OutputFormat::Text);
    assert_eq!(cli.output.verbosity, OutputVerbosity::Normal);
    assert!(!cli.exec.prevent_idle_sleep);
    assert!(cli.updates.check);
    assert!(cli.logging.level.is_none());
}

#[test]
fn resolves_cli_target_exec_and_output_settings() {
    let settings: SettingsLayer = parse_settings_layer(
        r#"
_version = 1

[cli.target]
type = "http"
url = "https://config.example.com"

[cli.target.tls]
cert = "cert.pem"
key = "key.pem"
ca = "ca.pem"

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
    .expect("fixture should parse");

    let cli = resolve_cli_from_file(&settings).expect("cli settings should resolve");

    let CliTargetSettings::Http { url, tls } = cli.target.expect("target") else {
        panic!("expected http target");
    };
    assert_eq!(url.as_source(), "https://config.example.com");
    let tls = tls.expect("tls");
    assert_eq!(tls.cert.as_source(), "cert.pem");
    assert_eq!(tls.key.as_source(), "key.pem");
    assert_eq!(tls.ca.as_source(), "ca.pem");

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
