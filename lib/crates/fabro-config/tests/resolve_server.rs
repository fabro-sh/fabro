use fabro_config::parse_settings_layer;
use fabro_types::settings::server::{ObjectStoreSettings, ServerListenSettings};
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
        Home::from_env().storage_dir().to_string_lossy()
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
                Home::from_env().storage_dir().to_string_lossy()
            );
        }
        ObjectStoreSettings::S3 { .. } => panic!("expected local artifact store by default"),
    }
    assert_eq!(settings.artifacts.prefix.as_source(), "artifacts");
}

#[test]
fn reports_tls_shape_errors_and_requires_valid_tls_for_mtls() {
    let file = parse(
        r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:32276"

[server.listen.tls]
cert = "/etc/fabro/server.pem"

[server.auth.api.mtls]
enabled = true
"#,
    );

    let errors = fabro_config::resolve_server_from_file(&file)
        .expect_err("incomplete tls config should fail");
    let rendered = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered.contains("server.listen.tls.key"));
    assert!(rendered.contains("server.listen.tls.ca"));
    assert!(rendered.contains("server.auth.api.mtls"));
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
