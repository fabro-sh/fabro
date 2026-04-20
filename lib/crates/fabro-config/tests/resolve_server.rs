use fabro_config::parse_settings_layer;
use fabro_config::user::default_storage_dir;
use fabro_types::settings::server::{
    GithubIntegrationStrategy, IpAllowEntry, ObjectStoreSettings, ServerListenSettings,
};
use fabro_types::settings::{InterpString, SettingsLayer};
use fabro_util::Home;

fn parse(source: &str) -> SettingsLayer {
    parse_settings_layer(source).expect("fixture should parse")
}

#[test]
fn resolves_server_defaults_from_empty_settings() {
    let settings = fabro_config::resolve_server_from_file(&SettingsLayer::default())
        .expect("empty settings should resolve");

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
fn parsing_rejects_inbound_listener_tls_configuration() {
    let err = fabro_config::parse_settings_layer(
        r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:32276"

[server.listen.tls]
cert = "/etc/fabro/server.pem"
"#,
    )
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

    let errors = fabro_config::resolve_server_from_file(&file)
        .expect_err("s3 config without bucket/region should fail");
    let rendered = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

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

    let settings =
        fabro_config::resolve_server_from_file(&file).expect("server settings should resolve");

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

    let settings =
        fabro_config::resolve_server_from_file(&file).expect("server settings should resolve");

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

    let settings =
        fabro_config::resolve_server_from_file(&file).expect("server settings should resolve");

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

    let settings = fabro_config::resolve_server_from_file(&file).expect("settings should resolve");

    assert!(settings.slatedb.disk_cache);
}

#[test]
fn resolves_empty_ip_allowlist_by_default() {
    let settings = fabro_config::resolve_server_from_file(&SettingsLayer::default())
        .expect("empty settings should resolve");

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

    let settings =
        fabro_config::resolve_server_from_file(&file).expect("server settings should resolve");

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

    let settings =
        fabro_config::resolve_server_from_file(&file).expect("server settings should resolve");
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

    let settings =
        fabro_config::resolve_server_from_file(&file).expect("server settings should resolve");
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

    let errors = fabro_config::resolve_server_from_file(&file)
        .expect_err("server_url webhook strategy should require server.api.url");
    let rendered = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

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

    let errors = fabro_config::resolve_server_from_file(&file)
        .expect_err("configured webhook strategy should require server.integrations.github.app_id");
    let rendered = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

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

    let errors =
        fabro_config::resolve_server_from_file(&file).expect_err("invalid CIDR should fail");
    let rendered = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

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

    let errors = fabro_config::resolve_server_from_file(&file)
        .expect_err("github_meta_hooks should be rejected outside github webhooks");
    let rendered = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

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

    let errors = fabro_config::resolve_server_from_file(&file)
        .expect_err("unix allowlist without trusted proxies should fail");
    let rendered = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

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

    let errors = fabro_config::resolve_server_from_file(&file)
        .expect_err("unix github webhook allowlist without trusted proxies should fail");
    let rendered = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        rendered.contains("server.integrations.github.webhooks.ip_allowlist.trusted_proxy_count")
    );
}
