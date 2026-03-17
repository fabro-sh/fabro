use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::run::RunDefaults;

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum PermissionLevel {
    ReadOnly,
    ReadWrite,
    Full,
}

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
pub struct CliGitConfig {
    pub app_id: Option<String>,
    pub slug: Option<String>,
    pub client_id: Option<String>,
    #[serde(default)]
    pub author: crate::server::GitAuthorConfig,
}

fn default_upgrade_check() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CliConfig {
    pub mode: Option<ExecutionMode>,
    pub server: Option<ServerDefaults>,
    pub exec: Option<ExecDefaults>,
    pub git: Option<CliGitConfig>,
    #[serde(default)]
    pub prevent_idle_sleep: bool,
    #[serde(default)]
    pub verbose: bool,
    #[serde(default = "default_upgrade_check")]
    pub upgrade_check: bool,
    #[serde(default)]
    pub log: crate::server::LogConfig,
    #[serde(flatten)]
    pub run_defaults: RunDefaults,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            mode: Default::default(),
            server: Default::default(),
            exec: Default::default(),
            git: Default::default(),
            prevent_idle_sleep: false,
            verbose: false,
            upgrade_check: true,
            log: Default::default(),
            run_defaults: Default::default(),
        }
    }
}

impl CliConfig {
    pub fn app_id(&self) -> Option<&str> {
        self.git.as_ref().and_then(|g| g.app_id.as_deref())
    }

    pub fn slug(&self) -> Option<&str> {
        self.git.as_ref().and_then(|g| g.slug.as_deref())
    }

    pub fn client_id(&self) -> Option<&str> {
        self.git.as_ref().and_then(|g| g.client_id.as_deref())
    }

    pub fn git_author(&self) -> Option<&crate::server::GitAuthorConfig> {
        self.git.as_ref().map(|g| &g.author)
    }
}

/// Load CLI config from an explicit path or `~/.fabro/cli.toml`, returning defaults if the
/// default file doesn't exist. An explicit path that doesn't exist is an error.
pub fn load_cli_config(path: Option<&Path>) -> anyhow::Result<CliConfig> {
    crate::load_config_file(path, "cli.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{McpServerEntry, McpTransport};
    use std::collections::HashMap;

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
        let llm = config.run_defaults.llm.unwrap();
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
        assert_eq!(config.run_defaults.llm, None);
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

    #[test]
    fn parse_server_tls() {
        let toml = r#"
[server]
base_url = "https://arc.example.com:3000"

[server.tls]
cert = "~/.fabro/tls/client.crt"
key = "~/.fabro/tls/client.key"
ca = "~/.fabro/tls/ca.crt"
"#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        let tls = config.server.unwrap().tls.unwrap();
        assert_eq!(tls.cert, PathBuf::from("~/.fabro/tls/client.crt"));
        assert_eq!(tls.key, PathBuf::from("~/.fabro/tls/client.key"));
        assert_eq!(tls.ca, PathBuf::from("~/.fabro/tls/ca.crt"));
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
    fn parse_prevent_idle_sleep_true() {
        let config: CliConfig = toml::from_str("prevent_idle_sleep = true").unwrap();
        assert!(config.prevent_idle_sleep);
    }

    #[test]
    fn parse_prevent_idle_sleep_defaults_to_false() {
        let config: CliConfig = toml::from_str("").unwrap();
        assert!(!config.prevent_idle_sleep);
    }

    #[test]
    fn parse_verbose_true() {
        let config: CliConfig = toml::from_str("verbose = true").unwrap();
        assert!(config.verbose);
    }

    #[test]
    fn parse_log_level() {
        let toml = "[log]\nlevel = \"debug\"";
        let config: CliConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.log.level.as_deref(), Some("debug"));
    }

    #[test]
    fn parse_pull_request_enabled() {
        let toml = r#"
[pull_request]
enabled = true
"#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        let pr = config.run_defaults.pull_request.unwrap();
        assert!(pr.enabled);
    }

    #[test]
    fn parse_pull_request_absent() {
        let config: CliConfig = toml::from_str("").unwrap();
        assert_eq!(config.run_defaults.pull_request, None);
    }

    #[test]
    fn parse_git_config_with_app_id() {
        let toml = r#"
[git]
app_id = "12345"
slug = "my-app"

[git.author]
name = "fabro-bot"
email = "arc@test.com"
"#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        let git = config.git.unwrap();
        assert_eq!(git.app_id.as_deref(), Some("12345"));
        assert_eq!(git.slug.as_deref(), Some("my-app"));
        assert_eq!(git.author.name.as_deref(), Some("fabro-bot"));
    }

    #[test]
    fn parse_git_config_with_client_id() {
        let toml = r#"
[git]
app_id = "12345"
slug = "my-app"
client_id = "Iv1.abc123"
"#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.client_id(), Some("Iv1.abc123"));
        let git = config.git.unwrap();
        assert_eq!(git.client_id.as_deref(), Some("Iv1.abc123"));
    }

    #[test]
    fn parse_llm_with_provider_and_fallbacks() {
        let toml = r#"
[llm]
model = "claude-sonnet-4-5"
provider = "anthropic"
"#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        let llm = config.run_defaults.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(llm.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn parse_sandbox_config() {
        let toml = r#"
[sandbox]
provider = "daytona"
"#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        let sandbox = config.run_defaults.sandbox.unwrap();
        assert_eq!(sandbox.provider.as_deref(), Some("daytona"));
    }

    #[test]
    fn parse_mcp_stdio_server_with_env_and_timeouts() {
        let toml = r#"
[mcp_servers.filesystem]
type = "stdio"
command = ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/workspace"]
startup_timeout_secs = 15
tool_timeout_secs = 90

[mcp_servers.filesystem.env]
NODE_ENV = "production"
"#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.run_defaults.mcp_servers.len(), 1);
        let entry = &config.run_defaults.mcp_servers["filesystem"];
        assert_eq!(entry.startup_timeout_secs, 15);
        assert_eq!(entry.tool_timeout_secs, 90);
        match &entry.transport {
            McpTransport::Stdio { command, env } => {
                assert_eq!(
                    command,
                    &[
                        "npx",
                        "-y",
                        "@modelcontextprotocol/server-filesystem",
                        "/workspace"
                    ]
                );
                assert_eq!(env.get("NODE_ENV").unwrap(), "production");
            }
            _ => panic!("expected Stdio transport"),
        }
    }

    #[test]
    fn parse_mcp_http_server_with_headers() {
        let toml = r#"
[mcp_servers.sentry]
type = "http"
url = "https://mcp.sentry.dev/mcp"

[mcp_servers.sentry.headers]
Authorization = "Bearer sk-xxx"
"#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.run_defaults.mcp_servers.len(), 1);
        let entry = &config.run_defaults.mcp_servers["sentry"];
        match &entry.transport {
            McpTransport::Http { url, headers } => {
                assert_eq!(url, "https://mcp.sentry.dev/mcp");
                assert_eq!(headers.get("Authorization").unwrap(), "Bearer sk-xxx");
            }
            _ => panic!("expected Http transport"),
        }
    }

    #[test]
    fn parse_mcp_empty_backward_compat() {
        let config: CliConfig = toml::from_str("").unwrap();
        assert!(config.run_defaults.mcp_servers.is_empty());
    }

    #[test]
    fn parse_mcp_both_transports() {
        let toml = r#"
[mcp_servers.local]
type = "stdio"
command = ["python3", "server.py"]

[mcp_servers.remote]
type = "http"
url = "https://mcp.example.com"
"#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.run_defaults.mcp_servers.len(), 2);
        assert!(matches!(
            config.run_defaults.mcp_servers["local"].transport,
            McpTransport::Stdio { .. }
        ));
        assert!(matches!(
            config.run_defaults.mcp_servers["remote"].transport,
            McpTransport::Http { .. }
        ));
    }

    #[test]
    fn parse_mcp_defaults_applied_when_timeouts_omitted() {
        let toml = r#"
[mcp_servers.minimal]
type = "stdio"
command = ["echo"]
"#;
        let config: CliConfig = toml::from_str(toml).unwrap();
        let entry = &config.run_defaults.mcp_servers["minimal"];
        assert_eq!(entry.startup_timeout_secs, 10);
        assert_eq!(entry.tool_timeout_secs, 60);
    }

    #[test]
    fn mcp_server_entry_into_config() {
        let entry = McpServerEntry {
            transport: McpTransport::Stdio {
                command: vec!["node".into(), "server.js".into()],
                env: HashMap::new(),
            },
            startup_timeout_secs: 15,
            tool_timeout_secs: 90,
        };
        let config = entry.into_config("my-server".into());
        assert_eq!(config.name, "my-server");
        assert_eq!(config.startup_timeout_secs, 15);
        assert_eq!(config.tool_timeout_secs, 90);
    }

    #[test]
    fn parse_upgrade_check_false() {
        let config: CliConfig = toml::from_str("upgrade_check = false").unwrap();
        assert!(!config.upgrade_check);
    }

    #[test]
    fn parse_upgrade_check_default_true() {
        let config: CliConfig = toml::from_str("").unwrap();
        assert!(config.upgrade_check);
    }
}
