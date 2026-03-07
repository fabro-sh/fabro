use std::path::{Path, PathBuf};

use arc_workflows::cli::run_config::RunDefaults;
use arc_workflows::hook::HookConfig;
use serde::{Deserialize, Serialize};
use tracing::debug;

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthProvider {
    #[default]
    Github,
    InsecureDisabled,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize)]
pub struct AuthConfig {
    #[serde(default)]
    pub provider: AuthProvider,
    #[serde(default)]
    pub allowed_usernames: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiAuthStrategy {
    Jwt,
    Mtls,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct TlsConfig {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub ca: PathBuf,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct ApiConfig {
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub authentication_strategies: Vec<ApiAuthStrategy>,
    pub tls: Option<TlsConfig>,
}

fn default_base_url() -> String {
    "http://localhost:3000".to_string()
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            base_url: default_base_url(),
            authentication_strategies: Vec::new(),
            tls: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitProvider {
    #[default]
    Github,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize)]
pub struct GitConfig {
    #[serde(default)]
    pub provider: GitProvider,
    pub app_id: Option<String>,
    pub client_id: Option<String>,
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct WebConfig {
    #[serde(default = "default_web_url")]
    pub url: String,
    #[serde(default)]
    pub auth: AuthConfig,
}

fn default_web_url() -> String {
    "http://localhost:5173".to_string()
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            url: default_web_url(),
            auth: AuthConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize)]
pub struct FeatureFlags {
    #[serde(default)]
    pub session_sandboxes: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ServerConfig {
    pub data_dir: Option<PathBuf>,
    pub max_concurrent_runs: Option<usize>,
    #[serde(default)]
    pub web: WebConfig,
    #[serde(default)]
    pub api: ApiConfig,
    #[serde(default)]
    pub git: GitConfig,
    #[serde(default)]
    pub feature_flags: FeatureFlags,
    #[serde(flatten)]
    pub run_defaults: RunDefaults,
    #[serde(flatten)]
    pub hook_config: HookConfig,
}

/// Load server config from an explicit path or `~/.arc/server.toml`, returning defaults if the
/// default file doesn't exist. An explicit path that doesn't exist is an error.
pub fn load_server_config(path: Option<&Path>) -> anyhow::Result<ServerConfig> {
    if let Some(explicit) = path {
        debug!(path = %explicit.display(), "Loading server config from explicit path");
        let contents = std::fs::read_to_string(explicit)?;
        return Ok(toml::from_str(&contents)?);
    }

    let Some(home) = dirs::home_dir() else {
        debug!("No home directory found, using default server config");
        return Ok(ServerConfig::default());
    };
    let default_path = home.join(".arc").join("server.toml");
    debug!(path = %default_path.display(), "Loading server config");
    match std::fs::read_to_string(&default_path) {
        Ok(contents) => Ok(toml::from_str(&contents)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ServerConfig::default()),
        Err(e) => Err(e.into()),
    }
}

/// Resolve the data directory: config value > default `~/.arc`.
pub fn resolve_data_dir(config: &ServerConfig) -> PathBuf {
    if let Some(ref dir) = config.data_dir {
        return dir.clone();
    }
    dirs::home_dir()
        .map(|h| h.join(".arc"))
        .unwrap_or_else(|| PathBuf::from(".arc"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_with_data_dir() {
        let toml = r#"data_dir = "/custom/path""#;
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.data_dir, Some(PathBuf::from("/custom/path")));
    }

    #[test]
    fn parse_empty_config_defaults() {
        let toml = "";
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.data_dir, None);
    }

    #[test]
    fn resolve_data_dir_uses_config_value() {
        let config = ServerConfig {
            data_dir: Some(PathBuf::from("/my/data")),
            ..ServerConfig::default()
        };
        assert_eq!(resolve_data_dir(&config), PathBuf::from("/my/data"));
    }

    #[test]
    fn resolve_data_dir_defaults_to_home_arc() {
        let config = ServerConfig::default();
        let dir = resolve_data_dir(&config);
        // Should end with .arc
        assert!(
            dir.ends_with(".arc"),
            "expected path ending with .arc, got: {}",
            dir.display()
        );
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
[web]
url = "https://arc.example.com"

[web.auth]
provider = "github"
allowed_usernames = ["brynary", "alice"]

[api]
base_url = "http://example.com:8080"
authentication_strategies = ["jwt"]

[git]
provider = "github"
app_id = "12345"
client_id = "Iv1.abc123"
"#;
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.web.url, "https://arc.example.com");
        assert_eq!(config.web.auth.provider, AuthProvider::Github);
        assert_eq!(config.web.auth.allowed_usernames, vec!["brynary", "alice"]);
        assert_eq!(config.api.base_url, "http://example.com:8080");
        assert_eq!(
            config.api.authentication_strategies,
            vec![ApiAuthStrategy::Jwt]
        );
        assert_eq!(config.git.provider, GitProvider::Github);
        assert_eq!(config.git.app_id.as_deref(), Some("12345"));
        assert_eq!(config.git.client_id.as_deref(), Some("Iv1.abc123"));
    }

    #[test]
    fn parse_web_defaults() {
        let toml = "";
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.web.url, "http://localhost:5173");
        assert_eq!(config.web.auth.provider, AuthProvider::Github);
        assert!(config.web.auth.allowed_usernames.is_empty());
    }

    #[test]
    fn parse_api_defaults() {
        let toml = "";
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.api.base_url, "http://localhost:3000");
        assert!(config.api.authentication_strategies.is_empty());
        assert!(config.api.tls.is_none());
    }

    #[test]
    fn parse_git_config() {
        let toml = r#"
[git]
provider = "github"
app_id = "12345"
client_id = "Iv1.abc123"
"#;
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.git.provider, GitProvider::Github);
        assert_eq!(config.git.app_id.as_deref(), Some("12345"));
        assert_eq!(config.git.client_id.as_deref(), Some("Iv1.abc123"));
    }

    #[test]
    fn parse_git_defaults() {
        let toml = "";
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.git.provider, GitProvider::Github);
        assert_eq!(config.git.app_id, None);
        assert_eq!(config.git.client_id, None);
    }

    #[test]
    fn parse_config_with_run_defaults() {
        let toml = r#"
[llm]
model = "claude-haiku"
provider = "anthropic"

[sandbox]
provider = "daytona"

[vars]
repo_url = "https://github.com/org/repo"
"#;
        let config: ServerConfig = toml::from_str(toml).unwrap();
        let llm = config.run_defaults.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("claude-haiku"));
        assert_eq!(llm.provider.as_deref(), Some("anthropic"));
        let sandbox = config.run_defaults.sandbox.unwrap();
        assert_eq!(sandbox.provider.as_deref(), Some("daytona"));
        let vars = config.run_defaults.vars.unwrap();
        assert_eq!(vars["repo_url"], "https://github.com/org/repo");
    }

    #[test]
    fn parse_config_server_and_run_defaults_together() {
        let toml = r#"
[web.auth]
provider = "github"

[git]
provider = "github"
app_id = "123"

[llm]
model = "gpt-4"
"#;
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.web.auth.provider, AuthProvider::Github);
        assert_eq!(config.git.app_id.as_deref(), Some("123"));
        let llm = config.run_defaults.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("gpt-4"));
    }

    #[test]
    fn parse_insecure_disabled_auth_provider() {
        let toml = r#"
[web.auth]
provider = "insecure_disabled"
"#;
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.web.auth.provider, AuthProvider::InsecureDisabled);
    }

    #[test]
    fn parse_jwt_and_mtls_strategies() {
        let toml = r#"
[api]
authentication_strategies = ["jwt", "mtls"]

[api.tls]
cert = "~/.arc/certs/server.crt"
key = "~/.arc/certs/server.key"
ca = "~/.arc/certs/ca.crt"
"#;
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.api.authentication_strategies,
            vec![ApiAuthStrategy::Jwt, ApiAuthStrategy::Mtls]
        );
        let tls = config.api.tls.unwrap();
        assert_eq!(tls.cert, PathBuf::from("~/.arc/certs/server.crt"));
        assert_eq!(tls.key, PathBuf::from("~/.arc/certs/server.key"));
        assert_eq!(tls.ca, PathBuf::from("~/.arc/certs/ca.crt"));
    }

    #[test]
    fn parse_max_concurrent_runs() {
        let toml = r#"max_concurrent_runs = 8"#;
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.max_concurrent_runs, Some(8));
    }

    #[test]
    fn parse_max_concurrent_runs_defaults_to_none() {
        let toml = "";
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.max_concurrent_runs, None);
    }

    #[test]
    fn parse_empty_strategies() {
        let toml = r#"
[api]
authentication_strategies = []
"#;
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert!(config.api.authentication_strategies.is_empty());
    }

    #[test]
    fn parse_jwt_only_strategy() {
        let toml = r#"
[api]
authentication_strategies = ["jwt"]
"#;
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.api.authentication_strategies,
            vec![ApiAuthStrategy::Jwt]
        );
        assert!(config.api.tls.is_none());
    }

    #[test]
    fn load_server_config_from_explicit_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custom.toml");
        std::fs::write(&path, r#"max_concurrent_runs = 42"#).unwrap();
        let config = load_server_config(Some(&path)).unwrap();
        assert_eq!(config.max_concurrent_runs, Some(42));
    }

    #[test]
    fn load_server_config_explicit_path_missing_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.toml");
        let result = load_server_config(Some(&path));
        assert!(result.is_err());
    }

    #[test]
    fn parse_config_with_hooks() {
        let toml = r#"
[[hooks]]
event = "run_start"
command = "echo 'run starting'"

[[hooks]]
event = "stage_complete"
command = "echo 'stage done'"
matcher = "agent_loop"
"#;
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.hook_config.hooks.len(), 2);
        assert_eq!(
            config.hook_config.hooks[0].event,
            arc_workflows::hook::HookEvent::RunStart
        );
        assert_eq!(
            config.hook_config.hooks[0].command.as_deref(),
            Some("echo 'run starting'")
        );
        assert_eq!(
            config.hook_config.hooks[1].event,
            arc_workflows::hook::HookEvent::StageComplete
        );
        assert_eq!(
            config.hook_config.hooks[1].matcher.as_deref(),
            Some("agent_loop")
        );
    }

    #[test]
    fn parse_feature_flags() {
        let toml = "[feature_flags]\nsession_sandboxes = true";
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.feature_flags.session_sandboxes, true);
    }

    #[test]
    fn parse_feature_flags_defaults() {
        let toml = "";
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.feature_flags.session_sandboxes, false);
    }

    #[test]
    fn parse_config_without_hooks_defaults_empty() {
        let toml = "";
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert!(config.hook_config.hooks.is_empty());
    }

    #[test]
    fn parse_config_with_checkpoint_exclude_globs() {
        let toml = r#"
[checkpoint]
exclude_globs = ["**/node_modules/**", "**/.cache/**"]
"#;
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.run_defaults.checkpoint.exclude_globs,
            vec!["**/node_modules/**", "**/.cache/**"]
        );
    }

    #[test]
    fn parse_config_checkpoint_defaults_empty() {
        let toml = "";
        let config: ServerConfig = toml::from_str(toml).unwrap();
        assert!(config.run_defaults.checkpoint.exclude_globs.is_empty());
    }
}
