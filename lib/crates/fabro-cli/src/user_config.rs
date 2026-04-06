use std::path::{Path, PathBuf};

pub(crate) use fabro_config::user::*;

use anyhow::{Result, bail};
use fabro_config::ConfigLayer;
use fabro_types::Settings;
use tracing::debug;

use crate::args::{ServerConnectionArgs, ServerTargetArgs};

pub(crate) fn load_settings() -> anyhow::Result<Settings> {
    ConfigLayer::settings()?.resolve()
}

pub(crate) fn settings_layer_with_storage_dir(
    storage_dir: Option<&Path>,
) -> anyhow::Result<ConfigLayer> {
    let layer = ConfigLayer::settings()?;
    Ok(apply_storage_dir_override(layer, storage_dir))
}

pub(crate) fn load_settings_with_storage_dir(
    storage_dir: Option<&Path>,
) -> anyhow::Result<Settings> {
    settings_layer_with_storage_dir(storage_dir)?.resolve()
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

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ServerConnection {
    Local { storage_dir: PathBuf },
    Target(ServerTarget),
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

fn resolve_server_connection(
    args: &ServerConnectionArgs,
    settings: &Settings,
    use_config_target: bool,
) -> Result<ServerConnection> {
    let connection = if let Some(value) = args.server() {
        ServerConnection::Target(parse_server_target(
            value,
            settings
                .server
                .as_ref()
                .and_then(|server| server.tls.clone()),
        )?)
    } else if args.storage_dir_is_explicit() {
        let storage_dir = args.storage_dir().ok_or_else(|| {
            anyhow::anyhow!("--storage-dir flag was present but no value was parsed")
        })?;
        ServerConnection::Local {
            storage_dir: storage_dir.to_path_buf(),
        }
    } else if use_config_target {
        configured_server_target(settings)?.map_or_else(
            || ServerConnection::Local {
                storage_dir: settings.storage_dir(),
            },
            ServerConnection::Target,
        )
    } else {
        ServerConnection::Local {
            storage_dir: settings.storage_dir(),
        }
    };
    debug!(?connection, "Resolved server connection");
    Ok(connection)
}

pub(crate) fn exec_server_target(
    args: &ServerTargetArgs,
    settings: &Settings,
) -> Result<Option<ServerTarget>> {
    let target = explicit_server_target(args, settings)?;
    debug!(?target, "Resolved exec server target");
    Ok(target)
}

pub(crate) fn server_only_command_connection(
    args: &ServerTargetArgs,
    settings: &Settings,
) -> Result<ServerConnection> {
    let connection = if let Some(target) = explicit_server_target(args, settings)? {
        ServerConnection::Target(target)
    } else if let Some(target) = configured_server_target(settings)? {
        ServerConnection::Target(target)
    } else {
        ServerConnection::Local {
            storage_dir: settings.storage_dir(),
        }
    };
    debug!(?connection, "Resolved server-only command connection");
    Ok(connection)
}

pub(crate) fn model_server_connection(
    args: &ServerConnectionArgs,
    settings: &Settings,
) -> Result<ServerConnection> {
    resolve_server_connection(args, settings, true)
}

pub(crate) fn server_backed_command_connection(
    args: &ServerConnectionArgs,
    settings: &Settings,
) -> Result<ServerConnection> {
    resolve_server_connection(args, settings, true)
}

pub(crate) fn build_server_client(
    tls: Option<&ClientTlsSettings>,
) -> anyhow::Result<reqwest::Client> {
    let Some(tls) = tls else {
        return Ok(reqwest::Client::new());
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

    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .identity(identity)
        .add_root_certificate(ca_cert)
        .build()?;

    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{ServerConnectionArgs, ServerTargetArgs};

    fn server_target_args(value: Option<&str>) -> ServerTargetArgs {
        ServerTargetArgs {
            server: value.map(str::to_string),
        }
    }

    fn server_connection_args(
        storage_dir: Option<&str>,
        server: Option<&str>,
    ) -> ServerConnectionArgs {
        ServerConnectionArgs {
            storage_dir: storage_dir.map(PathBuf::from),
            server: server.map(str::to_string),
            storage_dir_explicit: storage_dir.is_some(),
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
    fn model_uses_configured_server_target() {
        let settings = Settings {
            server: Some(ServerSettings {
                target: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..Settings::default()
        };
        assert_eq!(
            model_server_connection(&server_connection_args(None, None), &settings).unwrap(),
            ServerConnection::Target(ServerTarget::HttpUrl {
                api_url: "https://config.example.com".to_string(),
                tls: None,
            })
        );
    }

    #[test]
    fn server_only_command_uses_configured_server_target() {
        let settings = Settings {
            server: Some(ServerSettings {
                target: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..Settings::default()
        };
        assert_eq!(
            server_only_command_connection(&server_target_args(None), &settings).unwrap(),
            ServerConnection::Target(ServerTarget::HttpUrl {
                api_url: "https://config.example.com".to_string(),
                tls: None,
            })
        );
    }

    #[test]
    fn server_only_command_explicit_target_overrides_config_target() {
        let settings = Settings {
            server: Some(ServerSettings {
                target: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..Settings::default()
        };
        assert_eq!(
            server_only_command_connection(
                &server_target_args(Some("https://cli.example.com")),
                &settings,
            )
            .unwrap(),
            ServerConnection::Target(ServerTarget::HttpUrl {
                api_url: "https://cli.example.com".to_string(),
                tls: None,
            })
        );
    }

    #[test]
    fn server_only_command_defaults_to_local_storage_dir() {
        let settings = Settings {
            storage_dir: Some(PathBuf::from("/tmp/fabro")),
            ..Settings::default()
        };
        assert_eq!(
            server_only_command_connection(&server_target_args(None), &settings).unwrap(),
            ServerConnection::Local {
                storage_dir: PathBuf::from("/tmp/fabro"),
            }
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
            model_server_connection(
                &server_connection_args(None, Some("https://cli.example.com")),
                &settings,
            )
            .unwrap(),
            ServerConnection::Target(ServerTarget::HttpUrl {
                api_url: "https://cli.example.com".to_string(),
                tls: None,
            })
        );
    }

    #[test]
    fn storage_dir_suppresses_configured_remote_target() {
        let settings = Settings {
            server: Some(ServerSettings {
                target: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..Settings::default()
        };
        assert_eq!(
            model_server_connection(&server_connection_args(Some("/tmp/fabro"), None), &settings)
                .unwrap(),
            ServerConnection::Local {
                storage_dir: PathBuf::from("/tmp/fabro"),
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
