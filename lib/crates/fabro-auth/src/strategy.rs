use async_trait::async_trait;
use fabro_model::Provider;

use crate::context::{AuthContextRequest, AuthContextResponse};
use crate::credential::{AuthCredential, OAuthConfig};
use crate::strategies::api_key::ApiKeyStrategy;
use crate::strategies::codex_device::CodexDeviceStrategy;

pub const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const CODEX_AUTH_URL: &str = "https://auth.openai.com";
pub const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

#[async_trait]
pub trait AuthStrategy: Send {
    async fn init(&mut self) -> anyhow::Result<AuthContextRequest>;
    async fn complete(&mut self, response: AuthContextResponse) -> anyhow::Result<AuthCredential>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethod {
    ApiKey,
    CodexDevice(OAuthConfig),
}

#[must_use]
pub fn codex_oauth_config() -> OAuthConfig {
    OAuthConfig {
        auth_url: CODEX_AUTH_URL.to_string(),
        token_url: CODEX_TOKEN_URL.to_string(),
        client_id: CODEX_CLIENT_ID.to_string(),
        scopes: vec![
            "openid".to_string(),
            "profile".to_string(),
            "email".to_string(),
            "offline_access".to_string(),
        ],
        redirect_uri: Some(format!("{CODEX_AUTH_URL}/deviceauth/callback")),
        use_pkce: false,
    }
}

#[must_use]
pub fn strategy_for(provider: Provider, method: AuthMethod) -> Box<dyn AuthStrategy> {
    match method {
        AuthMethod::ApiKey => Box::new(ApiKeyStrategy::new(provider)),
        AuthMethod::CodexDevice(config) => {
            assert_eq!(
                provider,
                Provider::OpenAi,
                "Codex device auth is only supported for OpenAI"
            );
            Box::new(CodexDeviceStrategy::new(config))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::AuthContextRequest;

    #[test]
    fn codex_oauth_config_has_expected_defaults() {
        let config = codex_oauth_config();
        assert_eq!(config.auth_url, CODEX_AUTH_URL);
        assert_eq!(config.token_url, CODEX_TOKEN_URL);
        assert_eq!(config.client_id, CODEX_CLIENT_ID);
        assert!(!config.use_pkce);
        assert!(config.scopes.contains(&"offline_access".to_string()));
    }

    #[tokio::test]
    async fn api_key_strategy_uses_provider_env_names() {
        let mut strategy = ApiKeyStrategy::new(Provider::Anthropic);
        let request = strategy.init().await.unwrap();
        assert_eq!(
            request,
            AuthContextRequest::ApiKey {
                provider: Provider::Anthropic,
                env_var_names: vec!["ANTHROPIC_API_KEY".to_string()],
            }
        );
    }
}
