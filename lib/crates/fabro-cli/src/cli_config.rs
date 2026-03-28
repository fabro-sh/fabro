#[allow(unused_imports)]
pub(crate) use fabro_config::cli::*;

use std::path::Path;

use fabro_config::FabroSettings;
use fabro_config::cli::load_cli_config;

#[cfg(feature = "server")]
use tracing::debug;

pub(crate) fn load_cli_settings(path: Option<&Path>) -> anyhow::Result<FabroSettings> {
    load_cli_config(path)?.try_into()
}

#[cfg(feature = "server")]
#[derive(Debug, PartialEq)]
pub struct ResolvedMode {
    pub mode: ExecutionMode,
    pub server_base_url: String,
    pub tls: Option<ClientTlsSettings>,
}

#[cfg(feature = "server")]
const DEFAULT_SERVER_URL: &str = "http://localhost:3000";

#[cfg(feature = "server")]
pub fn resolve_mode(
    cli_mode: Option<ExecutionMode>,
    cli_server_url: Option<&str>,
    config: &FabroSettings,
) -> ResolvedMode {
    let mode = cli_mode.or_else(|| config.mode.clone()).unwrap_or_default();

    let server_defaults = config.server.as_ref();

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
pub fn build_server_client(tls: Option<&ClientTlsSettings>) -> anyhow::Result<reqwest::Client> {
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
    use std::path::PathBuf;

    use super::*;

    // --- resolve_mode precedence ---

    #[test]
    fn resolve_mode_defaults_to_standalone() {
        let config = FabroSettings::default();
        let resolved = resolve_mode(None, None, &config);
        assert_eq!(resolved.mode, ExecutionMode::Standalone);
        assert_eq!(resolved.server_base_url, DEFAULT_SERVER_URL);
        assert_eq!(resolved.tls, None);
    }

    #[test]
    fn resolve_mode_config_overrides_default() {
        let config = FabroSettings {
            mode: Some(ExecutionMode::Server),
            server: Some(ServerSettings {
                base_url: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..FabroSettings::default()
        };
        let resolved = resolve_mode(None, None, &config);
        assert_eq!(resolved.mode, ExecutionMode::Server);
        assert_eq!(resolved.server_base_url, "https://config.example.com");
    }

    #[test]
    fn resolve_mode_cli_overrides_config() {
        let config = FabroSettings {
            mode: Some(ExecutionMode::Standalone),
            server: Some(ServerSettings {
                base_url: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..FabroSettings::default()
        };
        let resolved = resolve_mode(
            Some(ExecutionMode::Server),
            Some("https://cli.example.com"),
            &config,
        );
        assert_eq!(resolved.mode, ExecutionMode::Server);
        assert_eq!(resolved.server_base_url, "https://cli.example.com");
    }

    #[test]
    fn resolve_mode_cli_url_overrides_config_url() {
        let config = FabroSettings {
            server: Some(ServerSettings {
                base_url: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..FabroSettings::default()
        };
        let resolved = resolve_mode(None, Some("https://cli.example.com"), &config);
        assert_eq!(resolved.server_base_url, "https://cli.example.com");
    }

    #[test]
    fn resolve_mode_tls_from_config() {
        let tls = ClientTlsSettings {
            cert: PathBuf::from("cert.pem"),
            key: PathBuf::from("key.pem"),
            ca: PathBuf::from("ca.pem"),
        };
        let config = FabroSettings {
            server: Some(ServerSettings {
                base_url: None,
                tls: Some(tls.clone()),
            }),
            ..FabroSettings::default()
        };
        let resolved = resolve_mode(None, None, &config);
        assert_eq!(resolved.tls, Some(tls));
    }
}
