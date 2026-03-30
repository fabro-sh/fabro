#[cfg(feature = "server")]
use std::path::Path;

#[allow(unused_imports)]
pub(crate) use fabro_config::user::*;

use fabro_config::ConfigLayer;
use fabro_config::FabroSettings;

use crate::args::GlobalArgs;

#[cfg(feature = "server")]
use tracing::debug;

pub(crate) fn load_user_settings() -> anyhow::Result<FabroSettings> {
    ConfigLayer::user()?.resolve()
}

pub(crate) fn user_layer_with_globals(globals: &GlobalArgs) -> anyhow::Result<ConfigLayer> {
    let layer = ConfigLayer::user()?;
    Ok(apply_global_overrides(layer, globals))
}

pub(crate) fn load_user_settings_with_globals(
    globals: &GlobalArgs,
) -> anyhow::Result<FabroSettings> {
    user_layer_with_globals(globals)?.resolve()
}

pub(crate) fn apply_global_overrides(mut layer: ConfigLayer, globals: &GlobalArgs) -> ConfigLayer {
    if let Some(dir) = &globals.storage_dir {
        layer.storage_dir = Some(dir.clone());
        layer.mode = Some(ExecutionMode::Standalone);
    }

    #[cfg(feature = "server")]
    if let Some(url) = &globals.server_url {
        layer.server.get_or_insert_with(Default::default).base_url = Some(url.clone());
        layer.mode = Some(ExecutionMode::Server);
    }

    layer
}

#[cfg(feature = "server")]
#[derive(Debug, PartialEq)]
pub(crate) struct ResolvedMode {
    pub mode: ExecutionMode,
    pub server_base_url: String,
    pub tls: Option<ClientTlsSettings>,
}

#[cfg(feature = "server")]
const DEFAULT_SERVER_URL: &str = "http://localhost:3000";

#[cfg(feature = "server")]
pub(crate) fn resolve_mode(
    cli_storage_dir: Option<&Path>,
    cli_server_url: Option<&str>,
    settings: &FabroSettings,
) -> ResolvedMode {
    let mode = if cli_server_url.is_some() {
        ExecutionMode::Server
    } else if cli_storage_dir.is_some() {
        ExecutionMode::Standalone
    } else {
        settings.mode.clone().unwrap_or_default()
    };

    let server_defaults = settings.server.as_ref();

    let server_base_url = cli_server_url
        .map(String::from)
        .or_else(|| server_defaults.and_then(|s| s.base_url.clone()))
        .unwrap_or_else(|| DEFAULT_SERVER_URL.to_string());

    let tls = server_defaults.and_then(|s| s.tls.clone());

    debug!(mode = ?mode, base_url = %server_base_url, tls = tls.is_some(), "CLI mode resolved");

    ResolvedMode {
        mode,
        server_base_url,
        tls,
    }
}

#[cfg(feature = "server")]
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

#[cfg(all(test, feature = "server"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    // --- resolve_mode precedence ---

    #[test]
    fn resolve_mode_defaults_to_standalone() {
        let settings = FabroSettings::default();
        let resolved = resolve_mode(None, None, &settings);
        assert_eq!(resolved.mode, ExecutionMode::Standalone);
        assert_eq!(resolved.server_base_url, DEFAULT_SERVER_URL);
        assert_eq!(resolved.tls, None);
    }

    #[test]
    fn resolve_mode_storage_dir_forces_standalone() {
        let settings = FabroSettings {
            mode: Some(ExecutionMode::Server),
            ..FabroSettings::default()
        };
        let resolved = resolve_mode(Some(Path::new("/tmp/fabro")), None, &settings);
        assert_eq!(resolved.mode, ExecutionMode::Standalone);
    }

    #[test]
    fn resolve_mode_server_url_forces_server() {
        let settings = FabroSettings {
            mode: Some(ExecutionMode::Standalone),
            server: Some(ServerSettings {
                base_url: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..FabroSettings::default()
        };
        let resolved = resolve_mode(None, Some("https://cli.example.com"), &settings);
        assert_eq!(resolved.mode, ExecutionMode::Server);
        assert_eq!(resolved.server_base_url, "https://cli.example.com");
    }

    #[test]
    fn resolve_mode_config_overrides_default() {
        let settings = FabroSettings {
            mode: Some(ExecutionMode::Server),
            server: Some(ServerSettings {
                base_url: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..FabroSettings::default()
        };
        let resolved = resolve_mode(None, None, &settings);
        assert_eq!(resolved.mode, ExecutionMode::Server);
        assert_eq!(resolved.server_base_url, "https://config.example.com");
    }

    #[test]
    fn resolve_mode_cli_url_overrides_config_url() {
        let settings = FabroSettings {
            server: Some(ServerSettings {
                base_url: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..FabroSettings::default()
        };
        let resolved = resolve_mode(None, Some("https://cli.example.com"), &settings);
        assert_eq!(resolved.server_base_url, "https://cli.example.com");
    }

    #[test]
    fn resolve_mode_tls_from_config() {
        let tls = ClientTlsSettings {
            cert: PathBuf::from("cert.pem"),
            key: PathBuf::from("key.pem"),
            ca: PathBuf::from("ca.pem"),
        };
        let settings = FabroSettings {
            server: Some(ServerSettings {
                base_url: None,
                tls: Some(tls.clone()),
            }),
            ..FabroSettings::default()
        };
        let resolved = resolve_mode(None, None, &settings);
        assert_eq!(resolved.tls, Some(tls));
    }
}
