use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
pub(crate) use fabro_config::user::*;
use fabro_types::settings::cli::CliTargetSettings;
use fabro_types::settings::{CliSettings, SettingsLayer};
use fabro_util::version::FABRO_VERSION;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Client-side TLS material for the CLI's remote server target.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct ClientTlsSettings {
    pub cert: PathBuf,
    pub key:  PathBuf,
    pub ca:   PathBuf,
}

use crate::args::ServerTargetArgs;

pub(crate) fn load_settings() -> anyhow::Result<SettingsLayer> {
    load_settings_with_config_and_storage_dir(None, None)
}

pub(crate) fn settings_layer_with_config_and_storage_dir(
    config_path: Option<&Path>,
    storage_dir: Option<&Path>,
) -> anyhow::Result<SettingsLayer> {
    let layer = load_settings_config(config_path)?;
    Ok(apply_storage_dir_override(layer, storage_dir))
}

pub(crate) fn settings_layer_with_storage_dir(
    storage_dir: Option<&Path>,
) -> anyhow::Result<SettingsLayer> {
    settings_layer_with_config_and_storage_dir(None, storage_dir)
}

pub(crate) fn load_settings_with_storage_dir(
    storage_dir: Option<&Path>,
) -> anyhow::Result<SettingsLayer> {
    settings_layer_with_storage_dir(storage_dir)
}

pub(crate) fn load_settings_with_config_and_storage_dir(
    config_path: Option<&Path>,
    storage_dir: Option<&Path>,
) -> anyhow::Result<SettingsLayer> {
    settings_layer_with_config_and_storage_dir(config_path, storage_dir)
}

fn render_resolve_errors(errors: Vec<fabro_config::ResolveError>) -> anyhow::Error {
    anyhow::anyhow!(
        "failed to resolve cli settings:\n{}",
        errors
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    )
}

pub(crate) fn resolve_cli_settings(file: &SettingsLayer) -> anyhow::Result<CliSettings> {
    fabro_config::resolve_cli_from_file(file).map_err(render_resolve_errors)
}

pub(crate) fn apply_storage_dir_override(
    mut layer: SettingsLayer,
    storage_dir: Option<&Path>,
) -> SettingsLayer {
    use fabro_types::settings::interp::InterpString;
    use fabro_types::settings::server::{ServerLayer, ServerStorageLayer};
    if let Some(dir) = storage_dir {
        let server = layer.server.get_or_insert_with(ServerLayer::default);
        let storage = server
            .storage
            .get_or_insert_with(ServerStorageLayer::default);
        storage.root = Some(InterpString::parse(&dir.display().to_string()));
    }

    layer
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ServerTarget {
    HttpUrl {
        api_url: String,
        tls:     Option<ClientTlsSettings>,
    },
    UnixSocket(PathBuf),
}

/// Pull the resolved CLI target configuration out of `[cli.target]`.
/// Returns `(target_string, tls)` where `target_string` is either an
/// http(s) URL or a unix socket path.
fn cli_target_from_settings(settings: &CliSettings) -> Option<(String, Option<ClientTlsSettings>)> {
    let target = settings.target.as_ref()?;
    match target {
        CliTargetSettings::Http { url, tls } => {
            let tls_settings = tls.as_ref().map(|tls| ClientTlsSettings {
                cert: PathBuf::from(tls.cert.as_source()),
                key:  PathBuf::from(tls.key.as_source()),
                ca:   PathBuf::from(tls.ca.as_source()),
            });
            Some((url.as_source(), tls_settings))
        }
        CliTargetSettings::Unix { path } => Some((path.as_source(), None)),
    }
}

fn configured_server_target(settings: &SettingsLayer) -> Result<Option<ServerTarget>> {
    let cli_settings = resolve_cli_settings(settings)?;
    let Some((value, tls)) = cli_target_from_settings(&cli_settings) else {
        return Ok(None);
    };
    parse_server_target(&value, tls).map(Some)
}

pub(crate) fn default_server_target() -> ServerTarget {
    ServerTarget::UnixSocket(default_socket_path())
}

pub(crate) fn storage_dir(settings: &SettingsLayer) -> anyhow::Result<PathBuf> {
    let resolved = fabro_config::resolve_server_from_file(settings).map_err(|errors| {
        anyhow::anyhow!(
            "failed to resolve server settings:\n{}",
            errors
                .into_iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        )
    })?;
    let resolved_root = resolved
        .storage
        .root
        .resolve(|name| std::env::var(name).ok())
        .map_err(|err| {
            anyhow::anyhow!(
                "failed to resolve {}: {err}",
                resolved.storage.root.as_source()
            )
        })?;
    Ok(PathBuf::from(resolved_root.value))
}

fn parse_server_target(value: &str, tls: Option<ClientTlsSettings>) -> Result<ServerTarget> {
    if value.starts_with("http://") || value.starts_with("https://") {
        return Ok(ServerTarget::HttpUrl {
            api_url: value.to_string(),
            tls,
        });
    }

    let path = Path::new(value);
    if path.is_absolute() {
        return Ok(ServerTarget::UnixSocket(path.to_path_buf()));
    }

    bail!("server target must be an http(s) URL or absolute Unix socket path")
}

fn explicit_server_target(
    args: &ServerTargetArgs,
    settings: &SettingsLayer,
) -> Result<Option<ServerTarget>> {
    let cli_settings = resolve_cli_settings(settings)?;
    args.as_deref()
        .map(|value| {
            parse_server_target(
                value,
                cli_target_from_settings(&cli_settings).and_then(|(_, tls)| tls),
            )
        })
        .transpose()
}

pub(crate) fn resolve_server_target(
    args: &ServerTargetArgs,
    settings: &SettingsLayer,
) -> Result<ServerTarget> {
    explicit_server_target(args, settings)?
        .or(configured_server_target(settings)?)
        .map_or_else(|| Ok(default_server_target()), Ok)
}

pub(crate) fn exec_server_target(
    args: &ServerTargetArgs,
    settings: &SettingsLayer,
) -> Result<Option<ServerTarget>> {
    let target = explicit_server_target(args, settings)?;
    debug!(?target, "Resolved exec server target");
    Ok(target)
}

pub(crate) fn cli_http_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().user_agent(format!("fabro-cli/{FABRO_VERSION}"))
}

pub(crate) fn build_server_client(
    tls: Option<&ClientTlsSettings>,
) -> anyhow::Result<reqwest::Client> {
    let Some(tls) = tls else {
        return Ok(cli_http_client_builder().build()?);
    };

    let cert_path = fabro_config::expand_tilde(&tls.cert);
    let key_path = fabro_config::expand_tilde(&tls.key);
    let ca_path = fabro_config::expand_tilde(&tls.ca);

    let cert_pem = std::fs::read(&cert_path)?;
    let key_pem = std::fs::read(&key_path)?;
    let ca_pem = std::fs::read(&ca_path)?;

    let mut identity_pem = cert_pem;
    identity_pem.push(b'\n');
    identity_pem.extend_from_slice(&key_pem);

    let identity = reqwest::Identity::from_pem(&identity_pem)?;
    let ca_cert = reqwest::Certificate::from_pem(&ca_pem)?;

    let client = cli_http_client_builder()
        .use_rustls_tls()
        .identity(identity)
        .add_root_certificate(ca_cert)
        .build()?;

    Ok(client)
}

#[cfg(test)]
mod tests {
    use fabro_config::parse_settings_layer;

    use super::*;
    use crate::args::ServerTargetArgs;

    fn server_target_args(value: Option<&str>) -> ServerTargetArgs {
        ServerTargetArgs {
            server: value.map(str::to_string),
        }
    }

    fn parse_v2(source: &str) -> SettingsLayer {
        parse_settings_layer(source).expect("fixture should parse")
    }

    #[test]
    fn exec_has_no_server_target_by_default() {
        let settings = SettingsLayer::default();
        assert_eq!(
            exec_server_target(&server_target_args(None), &settings).unwrap(),
            None
        );
    }

    #[test]
    fn exec_uses_cli_server_target() {
        let settings = SettingsLayer::default();
        assert_eq!(
            exec_server_target(
                &server_target_args(Some("https://cli.example.com")),
                &settings
            )
            .unwrap(),
            Some(ServerTarget::HttpUrl {
                api_url: "https://cli.example.com".to_string(),
                tls:     None,
            })
        );
    }

    #[test]
    fn exec_supports_explicit_unix_socket_target() {
        let settings = SettingsLayer::default();
        assert_eq!(
            exec_server_target(&server_target_args(Some("/tmp/fabro.sock")), &settings).unwrap(),
            Some(ServerTarget::UnixSocket(PathBuf::from("/tmp/fabro.sock")))
        );
    }

    #[test]
    fn exec_ignores_configured_server_target_without_cli_override() {
        let settings = parse_v2(
            r#"
_version = 1

[cli.target]
type = "http"
url = "https://config.example.com"
"#,
        );
        assert_eq!(
            exec_server_target(&server_target_args(None), &settings).unwrap(),
            None
        );
    }

    #[test]
    fn resolve_server_target_uses_configured_server_target() {
        let settings = parse_v2(
            r#"
_version = 1

[cli.target]
type = "http"
url = "https://config.example.com"
"#,
        );
        assert_eq!(
            resolve_server_target(&server_target_args(None), &settings).unwrap(),
            ServerTarget::HttpUrl {
                api_url: "https://config.example.com".to_string(),
                tls:     None,
            }
        );
    }

    #[test]
    fn resolve_server_target_explicit_target_overrides_config_target() {
        let settings = parse_v2(
            r#"
_version = 1

[cli.target]
type = "http"
url = "https://config.example.com"
"#,
        );
        assert_eq!(
            resolve_server_target(
                &server_target_args(Some("https://cli.example.com")),
                &settings
            )
            .unwrap(),
            ServerTarget::HttpUrl {
                api_url: "https://cli.example.com".to_string(),
                tls:     None,
            }
        );
    }

    #[test]
    fn resolve_server_target_defaults_to_default_unix_socket_target() {
        let settings = SettingsLayer::default();
        assert_eq!(
            resolve_server_target(&server_target_args(None), &settings).unwrap(),
            ServerTarget::UnixSocket(dirs::home_dir().unwrap().join(".fabro/fabro.sock"))
        );
    }

    #[test]
    fn explicit_server_target_overrides_config_target() {
        let settings = parse_v2(
            r#"
_version = 1

[cli.target]
type = "http"
url = "https://config.example.com"
"#,
        );
        assert_eq!(
            resolve_server_target(
                &server_target_args(Some("https://cli.example.com")),
                &settings
            )
            .unwrap(),
            ServerTarget::HttpUrl {
                api_url: "https://cli.example.com".to_string(),
                tls:     None,
            }
        );
    }

    #[test]
    fn remote_target_uses_tls_from_config() {
        let expected_tls = ClientTlsSettings {
            cert: PathBuf::from("cert.pem"),
            key:  PathBuf::from("key.pem"),
            ca:   PathBuf::from("ca.pem"),
        };
        let settings = parse_v2(
            r#"
_version = 1

[cli.target]
type = "http"
url = "https://config.example.com"

[cli.target.tls]
cert = "cert.pem"
key = "key.pem"
ca = "ca.pem"
"#,
        );
        assert_eq!(
            exec_server_target(
                &server_target_args(Some("https://cli.example.com")),
                &settings
            )
            .unwrap(),
            Some(ServerTarget::HttpUrl {
                api_url: "https://cli.example.com".to_string(),
                tls:     Some(expected_tls),
            })
        );
    }

    #[test]
    fn invalid_server_target_is_rejected() {
        let settings = SettingsLayer::default();
        let error =
            exec_server_target(&server_target_args(Some("fabro.internal")), &settings).unwrap_err();
        assert_eq!(
            error.to_string(),
            "server target must be an http(s) URL or absolute Unix socket path"
        );
    }
}
