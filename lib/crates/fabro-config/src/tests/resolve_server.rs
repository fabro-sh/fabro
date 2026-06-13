#![expect(
    clippy::disallowed_methods,
    reason = "sync test fixture setup and raw template source assertions; not on a Tokio path"
)]

use fabro_types::settings::InterpString;
use fabro_types::settings::server::{
    GithubIntegrationStrategy, LogDestination, ObjectStoreSettings, ServerAuthMethod,
    ServerListenSettings, ServerNamespace,
};
use fabro_util::Home;
use temp_env::with_var;

use crate::user::default_storage_dir;
use crate::{ServerSettingsBuilder, SettingsLayer};

fn parse(source: &str) -> SettingsLayer {
    let mut layer = source
        .parse::<SettingsLayer>()
        .expect("fixture should parse");
    layer.ensure_test_auth_methods();
    layer
}

fn empty_settings_with_auth_methods() -> SettingsLayer {
    SettingsLayer::test_default()
}

fn dev_token_auth_enabled(layer: &SettingsLayer) -> bool {
    layer
        .server
        .as_ref()
        .and_then(|server| server.auth.as_ref())
        .and_then(|auth| auth.methods.as_ref())
        .is_some_and(|methods| methods.contains(&ServerAuthMethod::DevToken))
}

fn resolve_server(file: &SettingsLayer) -> ServerNamespace {
    ServerSettingsBuilder::from_layer(file)
        .expect("server settings should resolve")
        .server
}

fn resolve_errors(error: fabro_config::Error) -> Vec<fabro_config::ResolveError> {
    match error {
        fabro_config::Error::Resolve { errors, .. } => errors,
        other => panic!("expected resolve error, got {other:#}"),
    }
}

fn render_resolve_error_lines(error: fabro_config::Error) -> String {
    resolve_errors(error)
        .into_iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn resolves_server_defaults_from_empty_settings() {
    let settings = resolve_server(&empty_settings_with_auth_methods());

    assert_eq!(
        settings.storage.root.as_source(),
        default_storage_dir().to_string_lossy()
    );
    assert!(settings.web.enabled);
    assert_eq!(settings.web.url.as_source(), "http://localhost:3000");
    assert_eq!(settings.scheduler.max_concurrent_runs, 5);
    assert_eq!(settings.logging.destination, LogDestination::File);

    match settings.listen {
        ServerListenSettings::Unix { path } => {
            assert_eq!(
                path.as_source(),
                Home::from_env().socket_path().to_string_lossy()
            );
        }
        ServerListenSettings::Tcp { .. } => panic!("expected default listen transport to be unix"),
    }

    match settings.artifacts.store {
        ObjectStoreSettings::Local { root } => {
            assert_eq!(
                root.as_source(),
                default_storage_dir()
                    .join("objects")
                    .join("artifacts")
                    .to_string_lossy()
            );
        }
        ObjectStoreSettings::S3 { .. } => panic!("expected local artifact store by default"),
    }
    assert_eq!(settings.artifacts.prefix.as_source(), "");

    match settings.slatedb.store {
        ObjectStoreSettings::Local { root } => {
            assert_eq!(
                root.as_source(),
                default_storage_dir()
                    .join("objects")
                    .join("slatedb")
                    .to_string_lossy()
            );
        }
        ObjectStoreSettings::S3 { .. } => panic!("expected local slatedb store by default"),
    }

    assert!(!settings.slatedb.disk_cache);
}

#[test]
fn resolved_server_integrations_disable_slack_when_config_is_absent() {
    let settings = resolve_server(&empty_settings_with_auth_methods());

    let integrations =
        serde_json::to_value(&settings.integrations).expect("integrations should serialize");

    assert_eq!(
        integrations,
        serde_json::json!({
            "github": {
                "enabled": false,
                "strategy": "token",
                "app_id": null,
                "client_id": null,
                "slug": null,
                "webhooks": null,
            },
            "slack": {
                "enabled": false,
                "default_channel": null,
            },
        })
    );
}

#[test]
fn resolved_server_integrations_enable_slack_when_config_is_present() {
    let settings = resolve_server(&parse(
        r"
_version = 1

[server.integrations.slack]
",
    ));

    assert!(settings.integrations.slack.enabled);
    assert!(settings.integrations.slack.default_channel.is_none());
}

#[test]
fn server_sandbox_defaults_all_providers_enabled() {
    let settings = ServerSettingsBuilder::from_toml(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]
"#,
    )
    .expect("server settings should resolve");

    let sandbox = settings.server.sandbox;
    assert!(sandbox.providers.local.enabled);
    assert!(sandbox.providers.docker.enabled);
    assert!(sandbox.providers.daytona.enabled);
}

#[test]
fn server_sandbox_allows_partial_provider_overrides() {
    let settings = ServerSettingsBuilder::from_toml(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.sandbox.providers.daytona]
enabled = false
"#,
    )
    .expect("server settings should resolve");

    let sandbox = settings.server.sandbox;
    assert!(sandbox.providers.local.enabled);
    assert!(sandbox.providers.docker.enabled);
    assert!(!sandbox.providers.daytona.enabled);
}

#[test]
fn parsing_rejects_unknown_server_sandbox_provider() {
    let err = ServerSettingsBuilder::from_toml(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.sandbox.providers.exe]
enabled = true
"#,
    )
    .expect_err("unknown sandbox provider should be rejected");

    assert!(
        err.to_string().contains("unknown field `exe`"),
        "unexpected error: {err}"
    );
}

#[test]
fn parsing_rejects_unknown_server_integrations() {
    let source = r"
_version = 1

[server.integrations.chatapp]
enabled = true
";

    let err = source
        .parse::<SettingsLayer>()
        .expect_err("unknown chat integration should be rejected");
    let message = err.to_string();
    assert!(
        message.contains("chatapp") || message.contains("unknown field"),
        "expected parse error for unknown chat provider, got: {message}"
    );
}

#[test]
fn resolves_server_logging_destination_from_settings() {
    let file = parse(
        r#"
_version = 1

[server.logging]
destination = "stdout"
"#,
    );

    let settings = resolve_server(&file);

    assert_eq!(settings.logging.destination, LogDestination::Stdout);
}

#[test]
fn parsing_rejects_invalid_server_log_filter() {
    let err = r#"
_version = 1

[server.logging]
level = "definitely not a filter"
"#
    .parse::<SettingsLayer>()
    .expect_err("invalid log filters should be rejected at parse time");

    assert!(
        err.to_string().contains("server.logging.level"),
        "unexpected error: {err}"
    );
}

#[test]
fn server_settings_from_layer_matches_namespace_resolvers() {
    let settings = parse(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.storage]
root = "/srv/fabro"
"#,
    );

    let context = fabro_config::ServerSettingsBuilder::from_layer(&settings)
        .expect("settings should resolve");

    assert_eq!(context.server.storage.root.as_source(), "/srv/fabro");
}

#[test]
fn server_settings_resolve_reads_default_settings_from_fabro_home() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("settings.toml"),
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.storage]
root = "/srv/from-home"
"#,
    )
    .unwrap();

    with_var("FABRO_HOME", Some(home.path()), || {
        let settings =
            fabro_config::ServerSettingsBuilder::load_default().expect("settings should resolve");
        assert_eq!(settings.server.storage.root.as_source(), "/srv/from-home");
    });
}

#[test]
fn parsing_rejects_inbound_listener_tls_configuration() {
    let err = r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:32276"

[server.listen.tls]
cert = "/etc/fabro/server.pem"
"#
    .parse::<SettingsLayer>()
    .expect_err("listener TLS should be rejected at parse time");

    assert!(err.to_string().contains("unknown field `tls`"));
}

#[test]
fn reports_s3_shape_errors() {
    let file = parse(
        r#"
_version = 1

[server.artifacts]
provider = "s3"

[server.artifacts.s3]
endpoint = "{{ env.S3_ENDPOINT }}"
"#,
    );

    let rendered = render_resolve_error_lines(
        ServerSettingsBuilder::from_layer(&file)
            .expect_err("s3 config without bucket/region should fail"),
    );

    assert!(rendered.contains("server.artifacts.s3.bucket"));
    assert!(rendered.contains("server.artifacts.s3.region"));
}

#[test]
fn preserves_interp_strings_in_resolved_server_settings() {
    let file = parse(
        r#"
_version = 1

[server.listen]
type = "unix"
path = "{{ env.FABRO_SOCKET }}"

[server.integrations.github]
app_id = "{{ env.GITHUB_APP_ID }}"
client_id = "{{ env.GITHUB_CLIENT_ID }}"
slug = "fabro-app"
"#,
    );

    let settings = resolve_server(&file);

    match settings.listen {
        ServerListenSettings::Unix { path } => {
            assert_eq!(path, InterpString::parse("{{ env.FABRO_SOCKET }}"));
        }
        ServerListenSettings::Tcp { .. } => panic!("expected unix listen transport"),
    }

    assert_eq!(
        settings.integrations.github.app_id,
        Some(InterpString::parse("{{ env.GITHUB_APP_ID }}"))
    );
    assert_eq!(
        settings.integrations.github.client_id,
        Some(InterpString::parse("{{ env.GITHUB_CLIENT_ID }}"))
    );
    assert_eq!(
        settings.integrations.github.slug,
        Some(InterpString::parse("fabro-app"))
    );
}

#[test]
fn resolves_github_integration_strategy_from_settings() {
    let file = parse(
        r#"
_version = 1

[server.integrations.github]
strategy = "app"
"#,
    );

    let settings = resolve_server(&file);

    assert_eq!(
        settings.integrations.github.strategy,
        GithubIntegrationStrategy::App
    );
}

#[test]
fn defaults_github_integration_strategy_to_token() {
    let file = parse(
        r"
_version = 1

[server.integrations.github]
enabled = true
",
    );

    let settings = resolve_server(&file);

    assert_eq!(
        settings.integrations.github.strategy,
        GithubIntegrationStrategy::Token
    );
}

#[test]
fn resolves_disk_cache_true_from_settings() {
    let file = parse(
        r"
_version = 1

[server.slatedb]
disk_cache = true
",
    );

    let settings = resolve_server(&file);

    assert!(settings.slatedb.disk_cache);
}

#[test]
fn parsing_rejects_removed_server_ip_allowlist() {
    let err = r#"
_version = 1

[server.ip_allowlist]
entries = ["10.0.0.0/8"]
"#
    .parse::<SettingsLayer>()
    .expect_err("removed server IP allowlist setting should be rejected");

    assert!(
        err.to_string().contains("ip_allowlist"),
        "unexpected error: {err}"
    );
}

#[test]
fn parsing_rejects_removed_github_webhook_ip_allowlist() {
    let err = r#"
_version = 1

[server.integrations.github.webhooks.ip_allowlist]
entries = ["github_meta_hooks"]
"#
    .parse::<SettingsLayer>()
    .expect_err("removed github webhook IP allowlist setting should be rejected");

    assert!(
        err.to_string().contains("ip_allowlist"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_server_url_webhook_strategy_without_server_api_url() {
    let file = parse(
        r#"
_version = 1

[server.integrations.github]
strategy = "app"

[server.integrations.github.webhooks]
strategy = "server_url"
"#,
    );

    let rendered = render_resolve_error_lines(
        ServerSettingsBuilder::from_layer(&file)
            .expect_err("server_url webhook strategy should require server.api.url"),
    );

    assert!(rendered.contains("server.api.url"));
}

#[test]
fn rejects_configured_webhook_strategy_without_github_app_id() {
    let file = parse(
        r#"
_version = 1

[server.integrations.github]
strategy = "app"

[server.integrations.github.webhooks]
strategy = "tailscale_funnel"
"#,
    );

    let rendered = render_resolve_error_lines(ServerSettingsBuilder::from_layer(&file).expect_err(
        "configured webhook strategy should require server.integrations.github.app_id",
    ));

    assert!(rendered.contains("server.integrations.github.app_id"));
}

#[test]
fn resolve_storage_root_defaults_with_minimal_server_auth_methods() {
    let settings = ServerSettingsBuilder::from_layer(&empty_settings_with_auth_methods())
        .expect("default server settings should resolve");
    assert_eq!(
        settings.server.storage.root.as_source(),
        default_storage_dir().to_string_lossy()
    );
}

#[test]
fn resolve_storage_root_prefers_explicit_root() {
    let file = parse(
        r#"
_version = 1

[server.storage]
root = "/srv/fabro"
"#,
    );
    let settings =
        ServerSettingsBuilder::from_layer(&file).expect("server settings should resolve");

    assert_eq!(settings.server.storage.root.as_source(), "/srv/fabro");
}

#[test]
fn resolve_storage_root_preserves_env_interpolation() {
    let file = parse(
        r#"
_version = 1

[server.storage]
root = "{{ env.FABRO_STORAGE_ROOT }}"
"#,
    );
    let settings =
        ServerSettingsBuilder::from_layer(&file).expect("server settings should resolve");

    assert_eq!(
        settings.server.storage.root,
        InterpString::parse("{{ env.FABRO_STORAGE_ROOT }}")
    );
}

#[test]
fn dev_token_auth_enabled_requires_explicit_dev_token_method() {
    let dev_token_only = parse(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]
"#,
    );
    let github_only = parse(
        r#"
_version = 1

[server.auth]
methods = ["github"]
"#,
    );
    let both = parse(
        r#"
_version = 1

[server.auth]
methods = ["dev-token", "github"]
"#,
    );

    assert!(dev_token_auth_enabled(&dev_token_only));
    assert!(!dev_token_auth_enabled(&github_only));
    assert!(dev_token_auth_enabled(&both));
    assert!(!dev_token_auth_enabled(&SettingsLayer::default()));
}
