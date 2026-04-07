use std::path::{Path, PathBuf};

pub(crate) use fabro_config::user::*;

use anyhow::{Result, bail};
use fabro_config::ConfigLayer;
use fabro_types::Settings;
use fabro_util::version::FABRO_VERSION;
use tracing::debug;

use crate::args::ServerTargetArgs;

pub(crate) fn load_settings() -> anyhow::Result<Settings> {
    load_settings_with_config_and_storage_dir(None, None)
}

pub(crate) fn settings_layer_with_config_and_storage_dir(
    config_path: Option<&Path>,
    storage_dir: Option<&Path>,
) -> anyhow::Result<ConfigLayer> {
    let layer = load_settings_config(config_path)?;
    Ok(apply_storage_dir_override(layer, storage_dir))
}

pub(crate) fn settings_layer_with_storage_dir(
    storage_dir: Option<&Path>,
) -> anyhow::Result<ConfigLayer> {
    settings_layer_with_config_and_storage_dir(None, storage_dir)
}

pub(crate) fn load_settings_with_storage_dir(
    storage_dir: Option<&Path>,
) -> anyhow::Result<Settings> {
    settings_layer_with_storage_dir(storage_dir)?.resolve()
}

pub(crate) fn load_settings_with_config_and_storage_dir(
    config_path: Option<&Path>,
    storage_dir: Option<&Path>,
) -> anyhow::Result<Settings> {
    settings_layer_with_config_and_storage_dir(config_path, storage_dir)?.resolve()
}

pub(crate) fn apply_storage_dir_override(
    mut layer: ConfigLayer,
    storage_dir: Option<&Path>,
) -> ConfigLayer {
    if let Some(dir) = storage_dir {
        layer.storage_dir = Some(dir.to_path_buf());
    }

    layer
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ServerTarget {
    HttpUrl {
        api_url: String,
        tls: Option<ClientTlsSettings>,
    },
    UnixSocket(PathBuf),
}

fn configured_server_target(settings: &Settings) -> Result<Option<ServerTarget>> {
    settings
        .server
        .as_ref()
        .and_then(|server| server.target.as_deref())
        .map(|value| {
            parse_server_target(
                value,
                settings
                    .server
                    .as_ref()
                    .and_then(|server| server.tls.clone()),
            )
        })
        .transpose()
}

pub(crate) fn default_server_target() -> ServerTarget {
    ServerTarget::UnixSocket(default_socket_path())
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
    settings: &Settings,
) -> Result<Option<ServerTarget>> {
    args.as_deref()
        .map(|value| {
            parse_server_target(
                value,
                settings
                    .server
                    .as_ref()
                    .and_then(|server| server.tls.clone()),
            )
        })
        .transpose()
}

pub(crate) fn resolve_server_target(
    args: &ServerTargetArgs,
    settings: &Settings,
) -> Result<ServerTarget> {
    explicit_server_target(args, settings)?
        .or(configured_server_target(settings)?)
        .map_or_else(|| Ok(default_server_target()), Ok)
}

pub(crate) fn exec_server_target(
    args: &ServerTargetArgs,
    settings: &Settings,
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
    use super::*;
    use crate::args::ServerTargetArgs;

    fn server_target_args(value: Option<&str>) -> ServerTargetArgs {
        ServerTargetArgs {
            server: value.map(str::to_string),
        }
    }

    #[test]
    fn exec_has_no_server_target_by_default() {
        let settings = Settings::default();
        assert_eq!(
            exec_server_target(&server_target_args(None), &settings).unwrap(),
            None
        );
    }

    #[test]
    fn exec_uses_cli_server_target() {
        let settings = Settings::default();
        assert_eq!(
            exec_server_target(
                &server_target_args(Some("https://cli.example.com")),
                &settings
            )
            .unwrap(),
            Some(ServerTarget::HttpUrl {
                api_url: "https://cli.example.com".to_string(),
                tls: None,
            })
        );
    }

    #[test]
    fn exec_supports_explicit_unix_socket_target() {
        let settings = Settings::default();
        assert_eq!(
            exec_server_target(&server_target_args(Some("/tmp/fabro.sock")), &settings).unwrap(),
            Some(ServerTarget::UnixSocket(PathBuf::from("/tmp/fabro.sock")))
        );
    }

    #[test]
    fn exec_ignores_configured_server_target_without_cli_override() {
        let settings = Settings {
            server: Some(ServerSettings {
                target: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..Settings::default()
        };
        assert_eq!(
            exec_server_target(&server_target_args(None), &settings).unwrap(),
            None
        );
    }

    #[test]
    fn resolve_server_target_uses_configured_server_target() {
        let settings = Settings {
            server: Some(ServerSettings {
                target: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..Settings::default()
        };
        assert_eq!(
            resolve_server_target(&server_target_args(None), &settings).unwrap(),
            ServerTarget::HttpUrl {
                api_url: "https://config.example.com".to_string(),
                tls: None,
            }
        );
    }

    #[test]
    fn resolve_server_target_explicit_target_overrides_config_target() {
        let settings = Settings {
            server: Some(ServerSettings {
                target: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..Settings::default()
        };
        assert_eq!(
            resolve_server_target(
                &server_target_args(Some("https://cli.example.com")),
                &settings
            )
            .unwrap(),
            ServerTarget::HttpUrl {
                api_url: "https://cli.example.com".to_string(),
                tls: None,
            }
        );
    }

    #[test]
    fn resolve_server_target_defaults_to_default_unix_socket_target() {
        let settings = Settings::default();
        assert_eq!(
            resolve_server_target(&server_target_args(None), &settings).unwrap(),
            ServerTarget::UnixSocket(dirs::home_dir().unwrap().join(".fabro/fabro.sock"))
        );
    }

    #[test]
    fn explicit_server_target_overrides_config_target() {
        let settings = Settings {
            server: Some(ServerSettings {
                target: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..Settings::default()
        };
        assert_eq!(
            resolve_server_target(
                &server_target_args(Some("https://cli.example.com")),
                &settings
            )
            .unwrap(),
            ServerTarget::HttpUrl {
                api_url: "https://cli.example.com".to_string(),
                tls: None,
            }
        );
    }

    #[test]
    fn remote_target_uses_tls_from_config() {
        let tls = ClientTlsSettings {
            cert: PathBuf::from("cert.pem"),
            key: PathBuf::from("key.pem"),
            ca: PathBuf::from("ca.pem"),
        };
        let settings = Settings {
            server: Some(ServerSettings {
                target: None,
                tls: Some(tls.clone()),
            }),
            ..Settings::default()
        };
        assert_eq!(
            exec_server_target(
                &server_target_args(Some("https://cli.example.com")),
                &settings
            )
            .unwrap(),
            Some(ServerTarget::HttpUrl {
                api_url: "https://cli.example.com".to_string(),
                tls: Some(tls),
            })
        );
    }

    #[test]
    fn invalid_server_target_is_rejected() {
        let settings = Settings::default();
        let error =
            exec_server_target(&server_target_args(Some("fabro.internal")), &settings).unwrap_err();
        assert_eq!(
            error.to_string(),
            "server target must be an http(s) URL or absolute Unix socket path"
        );
    }
}
