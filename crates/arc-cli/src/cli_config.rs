use std::path::{Path, PathBuf};

use arc_agent::cli::{OutputFormat, PermissionLevel};
use serde::Deserialize;
use tracing::debug;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMode {
    #[default]
    Standalone,
    Server,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ClientTlsConfig {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub ca: PathBuf,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ServerDefaults {
    pub base_url: Option<String>,
    pub tls: Option<ClientTlsConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ExecDefaults {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub permissions: Option<PermissionLevel>,
    pub output_format: Option<OutputFormat>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct LlmDefaults {
    pub model: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct CliGitConfig {
    #[serde(default)]
    pub author: arc_api::server_config::GitAuthorConfig,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct CliConfig {
    pub mode: Option<ExecutionMode>,
    pub server: Option<ServerDefaults>,
    pub exec: Option<ExecDefaults>,
    pub llm: Option<LlmDefaults>,
    pub git: Option<CliGitConfig>,
    #[serde(default)]
    pub log: arc_api::server_config::LogConfig,
}

#[derive(Debug, PartialEq)]
pub struct ResolvedMode {
    pub mode: ExecutionMode,
    pub server_base_url: String,
    pub tls: Option<ClientTlsConfig>,
}

const DEFAULT_SERVER_URL: &str = "http://localhost:3000";

pub fn resolve_mode(
    cli_mode: Option<ExecutionMode>,
    cli_server_url: Option<&str>,
    config: &CliConfig,
) -> ResolvedMode {
    let mode = cli_mode
        .or_else(|| config.mode.clone())
        .unwrap_or_default();

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

pub fn build_server_client(tls: Option<&ClientTlsConfig>) -> anyhow::Result<reqwest::Client> {
    let Some(tls) = tls else {
        return Ok(reqwest::Client::new());
    };

    let cert_path = arc_api::tls::expand_tilde(&tls.cert);
    let key_path = arc_api::tls::expand_tilde(&tls.key);
    let ca_path = arc_api::tls::expand_tilde(&tls.ca);

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

/// Load CLI config from an explicit path or `~/.arc/cli.toml`, returning defaults if the
/// default file doesn't exist. An explicit path that doesn't exist is an error.
pub fn load_cli_config(path: Option<&Path>) -> anyhow::Result<CliConfig> {
    if let Some(explicit) = path {
        debug!(path = %explicit.display(), "Loading CLI config from explicit path");
        let contents = std::fs::read_to_string(explicit)?;
        return Ok(toml::from_str(&contents)?);
    }

    let Some(home) = dirs::home_dir() else {
        debug!("No home directory found, using default CLI config");
        return Ok(CliConfig::default());
    };
    let default_path = home.join(".arc").join("cli.toml");
    debug!(path = %default_path.display(), "Loading CLI config");
    match std::fs::read_to_string(&default_path) {
        Ok(contents) => Ok(toml::from_str(&contents)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CliConfig::default()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_config_defaults() {
        let config: CliConfig = toml::from_str("").unwrap();
        assert_eq!(config, CliConfig::default());
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
[exec]
provider = "anthropic"
model = "claude-opus-4-6"
permissions = "read-write"
output_format = "text"

[llm]
model = "claude-sonnet-4-5"
"#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        let exec = config.exec.unwrap();
        assert_eq!(exec.provider.as_deref(), Some("anthropic"));
        assert_eq!(exec.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(exec.permissions, Some(PermissionLevel::ReadWrite));
        assert_eq!(exec.output_format, Some(OutputFormat::Text));
        let llm = config.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("claude-sonnet-4-5"));
    }

    #[test]
    fn parse_partial_exec_config() {
        let toml = r#"
[exec]
provider = "openai"
"#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        let exec = config.exec.unwrap();
        assert_eq!(exec.provider.as_deref(), Some("openai"));
        assert_eq!(exec.model, None);
        assert_eq!(exec.permissions, None);
        assert_eq!(exec.output_format, None);
        assert_eq!(config.llm, None);
    }

    #[test]
    fn load_cli_config_from_explicit_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custom.toml");
        std::fs::write(
            &path,
            r#"
[exec]
provider = "gemini"
model = "gemini-pro"
"#,
        )
        .unwrap();
        let config = load_cli_config(Some(&path)).unwrap();
        let exec = config.exec.unwrap();
        assert_eq!(exec.provider.as_deref(), Some("gemini"));
        assert_eq!(exec.model.as_deref(), Some("gemini-pro"));
    }

    #[test]
    fn load_cli_config_explicit_path_missing_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.toml");
        let result = load_cli_config(Some(&path));
        assert!(result.is_err());
    }

    // --- ExecutionMode parsing ---

    #[test]
    fn parse_mode_server() {
        let toml = r#"mode = "server""#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.mode, Some(ExecutionMode::Server));
    }

    #[test]
    fn parse_mode_standalone() {
        let toml = r#"mode = "standalone""#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.mode, Some(ExecutionMode::Standalone));
    }

    #[test]
    fn parse_mode_absent() {
        let config: CliConfig = toml::from_str("").unwrap();
        assert_eq!(config.mode, None);
    }

    // --- ServerDefaults parsing ---

    #[test]
    fn parse_server_base_url() {
        let toml = r#"
[server]
base_url = "https://arc.example.com:3000"
"#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        let server = config.server.unwrap();
        assert_eq!(
            server.base_url.as_deref(),
            Some("https://arc.example.com:3000")
        );
        assert_eq!(server.tls, None);
    }

    // --- ClientTlsConfig parsing ---

    #[test]
    fn parse_server_tls() {
        let toml = r#"
[server]
base_url = "https://arc.example.com:3000"

[server.tls]
cert = "~/.arc/tls/client.crt"
key = "~/.arc/tls/client.key"
ca = "~/.arc/tls/ca.crt"
"#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        let tls = config.server.unwrap().tls.unwrap();
        assert_eq!(tls.cert, PathBuf::from("~/.arc/tls/client.crt"));
        assert_eq!(tls.key, PathBuf::from("~/.arc/tls/client.key"));
        assert_eq!(tls.ca, PathBuf::from("~/.arc/tls/ca.crt"));
    }

    // --- resolve_mode precedence ---

    #[test]
    fn resolve_mode_defaults_to_standalone() {
        let config = CliConfig::default();
        let resolved = resolve_mode(None, None, &config);
        assert_eq!(resolved.mode, ExecutionMode::Standalone);
        assert_eq!(resolved.server_base_url, DEFAULT_SERVER_URL);
        assert_eq!(resolved.tls, None);
    }

    #[test]
    fn resolve_mode_config_overrides_default() {
        let config = CliConfig {
            mode: Some(ExecutionMode::Server),
            server: Some(ServerDefaults {
                base_url: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..CliConfig::default()
        };
        let resolved = resolve_mode(None, None, &config);
        assert_eq!(resolved.mode, ExecutionMode::Server);
        assert_eq!(resolved.server_base_url, "https://config.example.com");
    }

    #[test]
    fn resolve_mode_cli_overrides_config() {
        let config = CliConfig {
            mode: Some(ExecutionMode::Standalone),
            server: Some(ServerDefaults {
                base_url: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..CliConfig::default()
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
        let config = CliConfig {
            server: Some(ServerDefaults {
                base_url: Some("https://config.example.com".to_string()),
                tls: None,
            }),
            ..CliConfig::default()
        };
        let resolved = resolve_mode(None, Some("https://cli.example.com"), &config);
        assert_eq!(resolved.server_base_url, "https://cli.example.com");
    }

    #[test]
    fn parse_git_author_config() {
        let toml = r#"
[git.author]
name = "my-arc"
email = "me@local"
"#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        let git = config.git.unwrap();
        assert_eq!(git.author.name.as_deref(), Some("my-arc"));
        assert_eq!(git.author.email.as_deref(), Some("me@local"));
    }

    #[test]
    fn parse_git_author_absent() {
        let config: CliConfig = toml::from_str("").unwrap();
        assert_eq!(config.git, None);
    }

    #[test]
    fn parse_log_level() {
        let toml = "[log]\nlevel = \"debug\"";
        let config: CliConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.log.level.as_deref(), Some("debug"));
    }

    #[test]
    fn resolve_mode_tls_from_config() {
        let tls = ClientTlsConfig {
            cert: PathBuf::from("cert.pem"),
            key: PathBuf::from("key.pem"),
            ca: PathBuf::from("ca.pem"),
        };
        let config = CliConfig {
            server: Some(ServerDefaults {
                base_url: None,
                tls: Some(tls.clone()),
            }),
            ..CliConfig::default()
        };
        let resolved = resolve_mode(None, None, &config);
        assert_eq!(resolved.tls, Some(tls));
    }
}
