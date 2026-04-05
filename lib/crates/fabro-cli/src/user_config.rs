pub(crate) use fabro_config::user::*;

use fabro_config::ConfigLayer;
use fabro_types::Settings;
use tracing::debug;

use crate::args::GlobalArgs;

pub(crate) fn load_user_settings() -> anyhow::Result<Settings> {
    ConfigLayer::user()?.resolve()
}

pub(crate) fn user_layer_with_globals(globals: &GlobalArgs) -> anyhow::Result<ConfigLayer> {
    let layer = ConfigLayer::user()?;
    Ok(apply_global_overrides(layer, globals))
}

pub(crate) fn load_user_settings_with_globals(globals: &GlobalArgs) -> anyhow::Result<Settings> {
    user_layer_with_globals(globals)?.resolve()
}

pub(crate) fn apply_global_overrides(mut layer: ConfigLayer, globals: &GlobalArgs) -> ConfigLayer {
    if let Some(dir) = &globals.storage_dir {
        layer.storage_dir = Some(dir.clone());
    }

    if let Some(url) = &globals.server_url {
        layer.server.get_or_insert_with(Default::default).base_url = Some(url.clone());
    }

    layer
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ServerTarget {
    pub server_base_url: String,
    pub tls: Option<ClientTlsSettings>,
}

fn configured_server_target(settings: &Settings) -> Option<ServerTarget> {
    settings.server.as_ref().and_then(|server| {
        server.base_url.clone().map(|server_base_url| ServerTarget {
            server_base_url,
            tls: server.tls.clone(),
        })
    })
}

pub(crate) fn exec_server_target(
    globals: &GlobalArgs,
    settings: &Settings,
) -> Option<ServerTarget> {
    let target = globals
        .server_url
        .as_ref()
        .map(|server_base_url| ServerTarget {
            server_base_url: server_base_url.clone(),
            tls: settings
                .server
                .as_ref()
                .and_then(|server| server.tls.clone()),
        });
    debug!(has_target = target.is_some(), "Resolved exec server target");
    target
}

pub(crate) fn model_server_target(
    globals: &GlobalArgs,
    settings: &Settings,
) -> Option<ServerTarget> {
    let target = if let Some(server_base_url) = globals.server_url.as_ref() {
        Some(ServerTarget {
            server_base_url: server_base_url.clone(),
            tls: settings
                .server
                .as_ref()
                .and_then(|server| server.tls.clone()),
        })
    } else if globals.storage_dir.is_some() {
        None
    } else {
        configured_server_target(settings)
    };
    debug!(
        has_target = target.is_some(),
        "Resolved model server target"
    );
    target
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
    use std::path::PathBuf;

    use super::*;

    fn globals() -> GlobalArgs {
        GlobalArgs {
            json: false,
            debug: false,
            no_upgrade_check: false,
            quiet: false,
            verbose: false,
            storage_dir: None,
            server_url: None,
        }
    }

    #[test]
    fn exec_has_no_server_target_by_default() {
        let settings = Settings::default();
        assert_eq!(exec_server_target(&globals(), &settings), None);
    }

    #[test]
    fn exec_uses_cli_server_url() {
        let settings = Settings::default();
        let mut globals = globals();
        globals.server_url = Some("https://cli.example.com".to_string());
        assert_eq!(
            exec_server_target(&globals, &settings),
            Some(ServerTarget {
                server_base_url: "https://cli.example.com".to_string(),
                tls: None,
            })
        );
    }

    #[test]
    fn exec_ignores_configured_server_base_url_without_cli_server_url() {
        let settings = Settings {
            server: Some(ServerSettings {
                base_url: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..Settings::default()
        };
        assert_eq!(exec_server_target(&globals(), &settings), None);
    }

    #[test]
    fn model_uses_configured_server_base_url() {
        let settings = Settings {
            server: Some(ServerSettings {
                base_url: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..Settings::default()
        };
        assert_eq!(
            model_server_target(&globals(), &settings),
            Some(ServerTarget {
                server_base_url: "https://config.example.com".to_string(),
                tls: None,
            })
        );
    }

    #[test]
    fn model_cli_server_url_overrides_config_url() {
        let settings = Settings {
            server: Some(ServerSettings {
                base_url: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..Settings::default()
        };
        let mut globals = globals();
        globals.server_url = Some("https://cli.example.com".to_string());
        assert_eq!(
            model_server_target(&globals, &settings),
            Some(ServerTarget {
                server_base_url: "https://cli.example.com".to_string(),
                tls: None,
            })
        );
    }

    #[test]
    fn model_storage_dir_suppresses_configured_remote_target() {
        let settings = Settings {
            server: Some(ServerSettings {
                base_url: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..Settings::default()
        };
        let mut globals = globals();
        globals.storage_dir = Some(PathBuf::from("/tmp/fabro"));
        assert_eq!(model_server_target(&globals, &settings), None);
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
                base_url: None,
                tls: Some(tls.clone()),
            }),
            ..Settings::default()
        };
        let mut globals = globals();
        globals.server_url = Some("https://cli.example.com".to_string());
        assert_eq!(
            exec_server_target(&globals, &settings),
            Some(ServerTarget {
                server_base_url: "https://cli.example.com".to_string(),
                tls: Some(tls),
            })
        );
    }
}
