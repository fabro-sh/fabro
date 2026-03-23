use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::FabroConfig;

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
pub struct GitAuthorConfig {
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookStrategy {
    TailscaleFunnel,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct WebhookConfig {
    pub strategy: WebhookStrategy,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize)]
pub struct GitConfig {
    #[serde(default)]
    pub provider: GitProvider,
    pub app_id: Option<String>,
    pub client_id: Option<String>,
    pub slug: Option<String>,
    #[serde(default)]
    pub author: GitAuthorConfig,
    pub webhooks: Option<WebhookConfig>,
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
pub struct Features {
    #[serde(default)]
    pub session_sandboxes: bool,
    /// Experimental: enable automatic retro generation after workflow runs.
    #[serde(default)]
    pub retros: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct LogConfig {
    pub level: Option<String>,
}

/// Load server config from an explicit path or `~/.fabro/server.toml`, returning defaults if the
/// default file doesn't exist. An explicit path that doesn't exist is an error.
pub fn load_server_config(path: Option<&Path>) -> anyhow::Result<FabroConfig> {
    crate::load_config_file(path, "server.toml")
}

/// Resolve the data directory: config value > default `~/.fabro`.
pub fn resolve_data_dir(config: &FabroConfig) -> PathBuf {
    if let Some(ref dir) = config.data_dir {
        return dir.clone();
    }
    dirs::home_dir()
        .map(|h| h.join(".fabro"))
        .unwrap_or_else(|| PathBuf::from(".fabro"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_with_data_dir() {
        let toml = r#"data_dir = "/custom/path""#;
        let config: FabroConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.data_dir, Some(PathBuf::from("/custom/path")));
    }

    #[test]
    fn parse_empty_config_defaults() {
        let toml = "";
        let config: FabroConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.data_dir, None);
    }

    #[test]
    fn resolve_data_dir_uses_config_value() {
        let config = FabroConfig {
            data_dir: Some(PathBuf::from("/my/data")),
            ..FabroConfig::default()
        };
        assert_eq!(resolve_data_dir(&config), PathBuf::from("/my/data"));
    }

    #[test]
    fn resolve_data_dir_defaults_to_home_arc() {
        let config = FabroConfig::default();
        let dir = resolve_data_dir(&config);
        // Should end with .fabro
        assert!(
            dir.ends_with(".fabro"),
            "expected path ending with .fabro, got: {}",
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
        let config: FabroConfig = toml::from_str(toml).unwrap();
        let web = config.web.unwrap();
        assert_eq!(web.url, "https://arc.example.com");
        assert_eq!(web.auth.provider, AuthProvider::Github);
        assert_eq!(web.auth.allowed_usernames, vec!["brynary", "alice"]);
        let api = config.api.unwrap();
        assert_eq!(api.base_url, "http://example.com:8080");
        assert_eq!(api.authentication_strategies, vec![ApiAuthStrategy::Jwt]);
        let git = config.git.unwrap();
        assert_eq!(git.provider, GitProvider::Github);
        assert_eq!(git.app_id.as_deref(), Some("12345"));
        assert_eq!(git.client_id.as_deref(), Some("Iv1.abc123"));
    }

    #[test]
    fn parse_web_defaults() {
        let toml = "[web]\n";
        let config: FabroConfig = toml::from_str(toml).unwrap();
        let web = config.web.unwrap();
        assert_eq!(web.url, "http://localhost:5173");
        assert_eq!(web.auth.provider, AuthProvider::Github);
        assert!(web.auth.allowed_usernames.is_empty());
    }

    #[test]
    fn parse_api_defaults() {
        let toml = "[api]\n";
        let config: FabroConfig = toml::from_str(toml).unwrap();
        let api = config.api.unwrap();
        assert_eq!(api.base_url, "http://localhost:3000");
        assert!(api.authentication_strategies.is_empty());
        assert!(api.tls.is_none());
    }

    #[test]
    fn parse_git_config() {
        let toml = r#"
[git]
provider = "github"
app_id = "12345"
client_id = "Iv1.abc123"
"#;
        let config: FabroConfig = toml::from_str(toml).unwrap();
        let git = config.git.unwrap();
        assert_eq!(git.provider, GitProvider::Github);
        assert_eq!(git.app_id.as_deref(), Some("12345"));
        assert_eq!(git.client_id.as_deref(), Some("Iv1.abc123"));
    }

    #[test]
    fn parse_git_defaults() {
        let toml = "[git]\n";
        let config: FabroConfig = toml::from_str(toml).unwrap();
        let git = config.git.unwrap();
        assert_eq!(git.provider, GitProvider::Github);
        assert_eq!(git.app_id, None);
        assert_eq!(git.client_id, None);
        assert_eq!(git.author.name, None);
        assert_eq!(git.author.email, None);
    }

    #[test]
    fn parse_git_author_config() {
        let toml = r#"
[git.author]
name = "fabro-bot"
email = "fabro-bot@company.com"
"#;
        let config: FabroConfig = toml::from_str(toml).unwrap();
        let git = config.git.unwrap();
        assert_eq!(git.author.name.as_deref(), Some("fabro-bot"));
        assert_eq!(git.author.email.as_deref(), Some("fabro-bot@company.com"));
    }

    #[test]
    fn parse_git_author_partial() {
        let toml = r#"
[git.author]
name = "custom-name"
"#;
        let config: FabroConfig = toml::from_str(toml).unwrap();
        let git = config.git.unwrap();
        assert_eq!(git.author.name.as_deref(), Some("custom-name"));
        assert_eq!(git.author.email, None);
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
        let config: FabroConfig = toml::from_str(toml).unwrap();
        let llm = config.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("claude-haiku"));
        assert_eq!(llm.provider.as_deref(), Some("anthropic"));
        let sandbox = config.sandbox.unwrap();
        assert_eq!(sandbox.provider.as_deref(), Some("daytona"));
        let vars = config.vars.unwrap();
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
        let config: FabroConfig = toml::from_str(toml).unwrap();
        let web = config.web.unwrap();
        assert_eq!(web.auth.provider, AuthProvider::Github);
        let git = config.git.unwrap();
        assert_eq!(git.app_id.as_deref(), Some("123"));
        let llm = config.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("gpt-4"));
    }

    #[test]
    fn parse_insecure_disabled_auth_provider() {
        let toml = r#"
[web.auth]
provider = "insecure_disabled"
"#;
        let config: FabroConfig = toml::from_str(toml).unwrap();
        let web = config.web.unwrap();
        assert_eq!(web.auth.provider, AuthProvider::InsecureDisabled);
    }

    #[test]
    fn parse_jwt_and_mtls_strategies() {
        let toml = r#"
[api]
authentication_strategies = ["jwt", "mtls"]

[api.tls]
cert = "~/.fabro/certs/server.crt"
key = "~/.fabro/certs/server.key"
ca = "~/.fabro/certs/ca.crt"
"#;
        let config: FabroConfig = toml::from_str(toml).unwrap();
        let api = config.api.unwrap();
        assert_eq!(
            api.authentication_strategies,
            vec![ApiAuthStrategy::Jwt, ApiAuthStrategy::Mtls]
        );
        let tls = api.tls.unwrap();
        assert_eq!(tls.cert, PathBuf::from("~/.fabro/certs/server.crt"));
        assert_eq!(tls.key, PathBuf::from("~/.fabro/certs/server.key"));
        assert_eq!(tls.ca, PathBuf::from("~/.fabro/certs/ca.crt"));
    }

    #[test]
    fn parse_max_concurrent_runs() {
        let toml = r#"max_concurrent_runs = 8"#;
        let config: FabroConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.max_concurrent_runs, Some(8));
    }

    #[test]
    fn parse_max_concurrent_runs_defaults_to_none() {
        let toml = "";
        let config: FabroConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.max_concurrent_runs, None);
    }

    #[test]
    fn parse_empty_strategies() {
        let toml = r#"
[api]
authentication_strategies = []
"#;
        let config: FabroConfig = toml::from_str(toml).unwrap();
        let api = config.api.unwrap();
        assert!(api.authentication_strategies.is_empty());
    }

    #[test]
    fn parse_jwt_only_strategy() {
        let toml = r#"
[api]
authentication_strategies = ["jwt"]
"#;
        let config: FabroConfig = toml::from_str(toml).unwrap();
        let api = config.api.unwrap();
        assert_eq!(api.authentication_strategies, vec![ApiAuthStrategy::Jwt]);
        assert!(api.tls.is_none());
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
        let config: FabroConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.hooks.len(), 2);
        assert_eq!(config.hooks[0].event, crate::hook::HookEvent::RunStart);
        assert_eq!(
            config.hooks[0].command.as_deref(),
            Some("echo 'run starting'")
        );
        assert_eq!(config.hooks[1].event, crate::hook::HookEvent::StageComplete);
        assert_eq!(config.hooks[1].matcher.as_deref(), Some("agent_loop"));
    }

    #[test]
    fn parse_features() {
        let toml = "[features]\nsession_sandboxes = true";
        let config: FabroConfig = toml::from_str(toml).unwrap();
        let features = config.features.unwrap();
        assert!(features.session_sandboxes);
    }

    #[test]
    fn parse_features_defaults() {
        let toml = "";
        let config: FabroConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.features, None);
    }

    #[test]
    fn parse_config_without_hooks_defaults_empty() {
        let toml = "";
        let config: FabroConfig = toml::from_str(toml).unwrap();
        assert!(config.hooks.is_empty());
    }

    #[test]
    fn parse_config_with_checkpoint_exclude_globs() {
        let toml = r#"
[checkpoint]
exclude_globs = ["**/node_modules/**", "**/.cache/**"]
"#;
        let config: FabroConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.checkpoint.exclude_globs,
            vec!["**/node_modules/**", "**/.cache/**"]
        );
    }

    #[test]
    fn parse_config_checkpoint_defaults_empty() {
        let toml = "";
        let config: FabroConfig = toml::from_str(toml).unwrap();
        assert!(config.checkpoint.exclude_globs.is_empty());
    }

    #[test]
    fn parse_git_webhooks_config() {
        let toml = r#"
[git]
provider = "github"
app_id = "2993730"

[git.webhooks]
strategy = "tailscale_funnel"
"#;
        let config: FabroConfig = toml::from_str(toml).unwrap();
        let webhooks = config.git.unwrap().webhooks.unwrap();
        assert_eq!(webhooks.strategy, WebhookStrategy::TailscaleFunnel);
    }

    #[test]
    fn parse_git_webhooks_missing_is_none() {
        let toml = r#"
[git]
provider = "github"
app_id = "123"
"#;
        let config: FabroConfig = toml::from_str(toml).unwrap();
        assert!(config.git.unwrap().webhooks.is_none());
    }

    #[test]
    fn parse_log_config() {
        let toml = "[log]\nlevel = \"trace\"";
        let config: FabroConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.log.as_ref().and_then(|l| l.level.as_deref()),
            Some("trace")
        );
    }
}
