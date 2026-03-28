use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
#[serde(rename_all = "snake_case")]
pub enum AuthProvider {
    #[default]
    Github,
    InsecureDisabled,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize)]
pub struct AuthSettings {
    #[serde(default)]
    pub provider: AuthProvider,
    #[serde(default)]
    pub allowed_usernames: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize, crate::Combine)]
#[serde(rename_all = "snake_case")]
pub enum ApiAuthStrategy {
    Jwt,
    Mtls,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct TlsSettings {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub ca: PathBuf,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct ApiSettings {
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub authentication_strategies: Vec<ApiAuthStrategy>,
    pub tls: Option<TlsSettings>,
}

fn default_base_url() -> String {
    "http://localhost:3000".to_string()
}

impl Default for ApiSettings {
    fn default() -> Self {
        Self {
            base_url: default_base_url(),
            authentication_strategies: Vec::new(),
            tls: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize, crate::Combine)]
#[serde(rename_all = "snake_case")]
pub enum GitProvider {
    #[default]
    Github,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize)]
pub struct GitAuthorSettings {
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize, crate::Combine)]
#[serde(rename_all = "snake_case")]
pub enum WebhookStrategy {
    TailscaleFunnel,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct WebhookSettings {
    pub strategy: WebhookStrategy,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize)]
pub struct GitSettings {
    #[serde(default)]
    pub provider: GitProvider,
    pub app_id: Option<String>,
    pub client_id: Option<String>,
    pub slug: Option<String>,
    #[serde(default)]
    pub author: GitAuthorSettings,
    pub webhooks: Option<WebhookSettings>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct WebSettings {
    #[serde(default = "default_web_url")]
    pub url: String,
    #[serde(default)]
    pub auth: AuthSettings,
}

fn default_web_url() -> String {
    "http://localhost:5173".to_string()
}

impl Default for WebSettings {
    fn default() -> Self {
        Self {
            url: default_web_url(),
            auth: AuthSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize)]
pub struct FeaturesSettings {
    #[serde(default)]
    pub session_sandboxes: bool,
    #[serde(default)]
    pub retros: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct LogSettings {
    pub level: Option<String>,
}
