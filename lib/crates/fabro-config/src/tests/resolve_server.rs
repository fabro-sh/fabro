#![expect(
    clippy::disallowed_methods,
    reason = "sync test fixture setup; not on a Tokio path"
)]

use fabro_types::settings::InterpString;
use fabro_types::settings::server::{
    GithubIntegrationStrategy, IpAllowEntry, ObjectStoreSettings, ServerListenSettings,
};
use fabro_util::Home;
use temp_env::with_var;

use crate::resolve::dev_token_auth_enabled;
use crate::user::default_storage_dir;
use crate::{ServerSettingsBuilder, SettingsLayer};

fn parse(source: &str) -> SettingsLayer {
    let mut layer = source.parse::<SettingsLayer>().expect("fixture should parse");
    layer.ensure_test_auth_methods();
    layer
}

fn empty_settings_with_auth_methods() -> SettingsLayer {
    SettingsLayer::test_default()
}

fn resolve_server(file: &SettingsLayer) -> fabro_types::settings::ServerNamespace {
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
fn server_settings_from_layer_matches_namespace_resolvers() {
    let settings = parse(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.storage]
root = "/srv/fabro"

[features]
session_sandboxes = true
"#,
    );

    let context = fabro_config::ServerSettingsBuilder::from_layer(&settings)
        .expect("settings should resolve");
    let user_settings = fabro_config::UserSettingsBuilder::from_layer(&settings)
        .expect("user settings should resolve");

    assert_eq!(context.server.storage.root.as_source(), "/srv/fabro");
    assert!(context.features.session_sandboxes);
    assert_eq!(context.features, user_settings.features);
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

[features]
session_sandboxes = true
"#,
    )
    .unwrap();

    with_var("FABRO_HOME", Some(home.path()), || {
        let settings =
            fabro_config::ServerSettingsBuilder::load_default().expect("settings should resolve");
        assert_eq!(settings.server.storage.root.as_source(), "/srv/from-home");
        assert!(settings.features.session_sandboxes);
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
fn resolves_empty_ip_allowlist_by_default() {
    let settings = resolve_server(&empty_settings_with_auth_methods());

    assert!(settings.ip_allowlist.entries.is_empty());
    assert_eq!(settings.ip_allowlist.trusted_proxy_count, 0);
}

#[test]
fn resolves_global_ip_allowlist_entries_and_proxy_count() {
    let file = parse(
        r#"
_version = 1

[server.ip_allowlist]
entries = ["10.0.0.0/8", "2001:db8::/32", "192.0.2.42"]
trusted_proxy_count = 2
"#,
    );

    let settings = resolve_server(&file);

    assert_eq!(settings.ip_allowlist.entries, vec![
        IpAllowEntry::parse_literal("10.0.0.0/8").unwrap(),
        IpAllowEntry::parse_literal("2001:db8::/32").unwrap(),
        IpAllowEntry::parse_literal("192.0.2.42").unwrap(),
    ]);
    assert_eq!(settings.ip_allowlist.trusted_proxy_count, 2);
}

#[test]
fn resolves_github_webhook_ip_allowlist_overlay_with_inheritance() {
    let file = parse(
        r#"
_version = 1

[server.ip_allowlist]
entries = ["10.0.0.0/8"]
trusted_proxy_count = 2

[server.integrations.github.webhooks.ip_allowlist]
entries = ["github_meta_hooks"]
"#,
    );

    let settings = resolve_server(&file);
    let webhook_allowlist = settings
        .integrations
        .github
        .webhooks
        .expect("github webhooks settings should resolve")
        .ip_allowlist
        .expect("github webhook ip allowlist overlay should resolve");

    assert_eq!(
        webhook_allowlist.entries,
        Some(vec![IpAllowEntry::GitHubMetaHooks])
    );
    assert_eq!(webhook_allowlist.trusted_proxy_count, None);
}

#[test]
fn resolves_github_webhook_ip_allowlist_override_proxy_count() {
    let file = parse(
        r#"
_version = 1

[server.ip_allowlist]
entries = ["10.0.0.0/8"]
trusted_proxy_count = 2

[server.integrations.github.webhooks.ip_allowlist]
trusted_proxy_count = 3
"#,
    );

    let settings = resolve_server(&file);
    let webhook_allowlist = settings
        .integrations
        .github
        .webhooks
        .expect("github webhooks settings should resolve")
        .ip_allowlist
        .expect("github webhook ip allowlist overlay should resolve");

    assert_eq!(webhook_allowlist.entries, None);
    assert_eq!(webhook_allowlist.trusted_proxy_count, Some(3));
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
fn rejects_invalid_ip_allowlist_entry() {
    let file = parse(
        r#"
_version = 1

[server.ip_allowlist]
entries = ["10.0.0.0/33"]
"#,
    );

    let rendered = render_resolve_error_lines(
        ServerSettingsBuilder::from_layer(&file).expect_err("invalid CIDR should fail"),
    );

    assert!(rendered.contains("server.ip_allowlist.entries[0]"));
}

#[test]
fn rejects_github_meta_hooks_in_global_scope() {
    let file = parse(
        r#"
_version = 1

[server.ip_allowlist]
entries = ["github_meta_hooks"]
"#,
    );

    let rendered = render_resolve_error_lines(
        ServerSettingsBuilder::from_layer(&file)
            .expect_err("github_meta_hooks should be rejected outside github webhooks"),
    );

    assert!(rendered.contains("server.ip_allowlist.entries[0]"));
}

#[test]
fn rejects_unix_socket_allowlist_without_trusted_proxy() {
    let file = parse(
        r#"
_version = 1

[server.listen]
type = "unix"
path = "/tmp/fabro.sock"

[server.ip_allowlist]
entries = ["10.0.0.0/8"]
"#,
    );

    let rendered = render_resolve_error_lines(
        ServerSettingsBuilder::from_layer(&file)
            .expect_err("unix allowlist without trusted proxies should fail"),
    );

    assert!(rendered.contains("server.ip_allowlist.trusted_proxy_count"));
}

#[test]
fn rejects_unix_socket_github_webhook_allowlist_without_trusted_proxy() {
    let file = parse(
        r#"
_version = 1

[server.listen]
type = "unix"
path = "/tmp/fabro.sock"

[server.integrations.github.webhooks.ip_allowlist]
entries = ["github_meta_hooks"]
"#,
    );

    let rendered = render_resolve_error_lines(
        ServerSettingsBuilder::from_layer(&file)
            .expect_err("unix github webhook allowlist without trusted proxies should fail"),
    );

    assert!(
        rendered.contains("server.integrations.github.webhooks.ip_allowlist.trusted_proxy_count")
    );
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
