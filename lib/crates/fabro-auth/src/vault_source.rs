use std::sync::Arc;

use async_trait::async_trait;
use fabro_model::Provider;
use fabro_vault::Vault;
use tokio::sync::RwLock as AsyncRwLock;

use crate::credential_source::{CredentialSource, ResolvedCredentials};
use crate::{CredentialResolver, CredentialUsage, EnvLookup, ResolveError, ResolvedCredential};

#[derive(Clone)]
pub struct VaultCredentialSource {
    vault:    Arc<AsyncRwLock<Vault>>,
    resolver: CredentialResolver,
}

impl VaultCredentialSource {
    #[must_use]
    pub fn new(vault: Arc<AsyncRwLock<Vault>>) -> Self {
        let resolver = CredentialResolver::new(Arc::clone(&vault));
        Self { vault, resolver }
    }

    #[must_use]
    pub fn with_env_lookup<F>(vault: Arc<AsyncRwLock<Vault>>, env_lookup: F) -> Self
    where
        F: Fn(&str) -> Option<String> + Send + Sync + 'static,
    {
        let env_lookup: EnvLookup = Arc::new(env_lookup);
        let resolver = CredentialResolver::with_env_lookup(Arc::clone(&vault), env_lookup);
        Self { vault, resolver }
    }
}

impl std::fmt::Debug for VaultCredentialSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultCredentialSource")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl CredentialSource for VaultCredentialSource {
    async fn resolve(&self) -> anyhow::Result<ResolvedCredentials> {
        let mut credentials = Vec::new();
        let mut auth_issues = Vec::new();

        for provider in Provider::ALL {
            match self
                .resolver
                .resolve(*provider, CredentialUsage::ApiRequest)
                .await
            {
                Ok(ResolvedCredential::Api(credential)) => credentials.push(credential),
                Ok(ResolvedCredential::Cli(_)) | Err(ResolveError::NotConfigured(_)) => {}
                Err(err) => auth_issues.push((*provider, err)),
            }
        }

        Ok(ResolvedCredentials {
            credentials,
            auth_issues,
        })
    }

    async fn configured_providers(&self) -> Vec<Provider> {
        let vault = self.vault.read().await;
        self.resolver.configured_providers(&vault)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{Duration, Utc};
    use fabro_model::Provider;
    use fabro_vault::{SecretType, Vault};
    use tokio::sync::RwLock as AsyncRwLock;

    use super::VaultCredentialSource;
    use crate::credential::{AuthCredential, AuthDetails, OAuthConfig, OAuthTokens};
    use crate::{CredentialSource, ResolveError};

    fn api_key_credential(provider: Provider, key: &str) -> AuthCredential {
        AuthCredential {
            provider,
            details: AuthDetails::ApiKey {
                key: key.to_string(),
            },
        }
    }

    fn expired_openai_credential() -> AuthCredential {
        AuthCredential {
            provider: Provider::OpenAi,
            details:  AuthDetails::CodexOAuth {
                tokens:     OAuthTokens {
                    access_token:  "expired-access".to_string(),
                    refresh_token: Some("refresh-token".to_string()),
                    expires_at:    Utc::now() - Duration::hours(1),
                },
                config:     OAuthConfig {
                    auth_url:     "https://auth.openai.com".to_string(),
                    token_url:    "http://127.0.0.1:9/oauth/token".to_string(),
                    client_id:    "client".to_string(),
                    scopes:       vec!["openid".to_string()],
                    redirect_uri: Some("https://example.com/callback".to_string()),
                    use_pkce:     true,
                },
                account_id: Some("acct_123".to_string()),
            },
        }
    }

    #[tokio::test]
    async fn resolve_returns_credentials_and_auth_issues() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault
            .set(
                "openai_codex",
                &serde_json::to_string(&expired_openai_credential()).unwrap(),
                SecretType::Credential,
                None,
            )
            .unwrap();
        vault
            .set(
                "anthropic",
                &serde_json::to_string(&api_key_credential(Provider::Anthropic, "anthropic-key"))
                    .unwrap(),
                SecretType::Credential,
                None,
            )
            .unwrap();

        let source =
            VaultCredentialSource::with_env_lookup(Arc::new(AsyncRwLock::new(vault)), |_| None);

        let resolved = source.resolve().await.unwrap();

        assert_eq!(resolved.credentials.len(), 1);
        assert_eq!(resolved.credentials[0].provider, Provider::Anthropic);
        assert_eq!(resolved.auth_issues.len(), 1);
        assert!(matches!(
            resolved.auth_issues[0].1,
            ResolveError::RefreshFailed {
                provider: Provider::OpenAi,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn configured_providers_reads_from_vault_without_refreshing() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault
            .set(
                "openai",
                &serde_json::to_string(&api_key_credential(Provider::OpenAi, "openai-key"))
                    .unwrap(),
                SecretType::Credential,
                None,
            )
            .unwrap();
        vault
            .set(
                "anthropic",
                &serde_json::to_string(&api_key_credential(Provider::Anthropic, "anthropic-key"))
                    .unwrap(),
                SecretType::Credential,
                None,
            )
            .unwrap();
        let source =
            VaultCredentialSource::with_env_lookup(Arc::new(AsyncRwLock::new(vault)), |_| None);

        assert_eq!(source.configured_providers().await, vec![
            Provider::Anthropic,
            Provider::OpenAi
        ]);
    }
}
