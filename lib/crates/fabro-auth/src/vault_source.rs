use std::sync::Arc;

use async_trait::async_trait;
use fabro_model::{Catalog, ProviderId};
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

    #[must_use]
    pub fn vault_only(vault: Arc<AsyncRwLock<Vault>>) -> Self {
        Self::with_env_lookup(vault, |_| None)
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
    async fn resolve(&self, catalog: &Catalog) -> anyhow::Result<ResolvedCredentials> {
        let mut credentials = Vec::new();
        let mut auth_issues = Vec::new();

        for provider in catalog.providers() {
            match self
                .resolver
                .resolve(provider.id.clone(), CredentialUsage::ApiRequest, catalog)
                .await
            {
                Ok(ResolvedCredential::Api(credential)) => credentials.push(credential),
                Err(ResolveError::NotConfigured(_)) if provider.auth.is_some() => {}
                Err(err) => auth_issues.push((provider.id.clone(), err)),
            }
        }

        Ok(ResolvedCredentials {
            credentials,
            auth_issues,
        })
    }

    async fn configured_providers(&self, catalog: &Catalog) -> Vec<ProviderId> {
        let vault = self.vault.read().await;
        self.resolver.configured_providers(&vault, catalog)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{Duration, Utc};
    use fabro_model::{Catalog, ProviderId};
    use fabro_vault::Vault;
    use tokio::sync::RwLock as AsyncRwLock;

    use super::VaultCredentialSource;
    use crate::credential::{OAuthConfig, OAuthCredential, OAuthTokens};
    use crate::vault_ext::{vault_set_oauth, vault_set_token};
    use crate::{CredentialSource, ResolveError};

    fn expired_openai_credential() -> OAuthCredential {
        OAuthCredential {
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
        }
    }

    fn default_catalog() -> Catalog {
        Catalog::from_builtin().unwrap()
    }

    #[tokio::test]
    async fn resolve_returns_credentials_and_auth_issues() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_oauth(
            &mut vault,
            crate::OPENAI_CODEX_VAULT_SECRET_NAME,
            &expired_openai_credential(),
        )
        .unwrap();
        vault_set_token(&mut vault, "ANTHROPIC_API_KEY", "anthropic-key").unwrap();

        let source =
            VaultCredentialSource::with_env_lookup(Arc::new(AsyncRwLock::new(vault)), |_| None);
        let catalog = default_catalog();

        let resolved = source.resolve(&catalog).await.unwrap();

        assert_eq!(resolved.credentials.len(), 1);
        assert_eq!(resolved.credentials[0].provider, ProviderId::anthropic());
        assert_eq!(resolved.auth_issues.len(), 1);
        assert!(matches!(
            &resolved.auth_issues[0].1,
            ResolveError::RefreshFailed {
                provider,
                ..
            } if provider == &ProviderId::openai()
        ));
    }

    #[tokio::test]
    async fn configured_providers_reads_from_vault_without_refreshing() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_token(&mut vault, "OPENAI_API_KEY", "openai-key").unwrap();
        vault_set_token(&mut vault, "ANTHROPIC_API_KEY", "anthropic-key").unwrap();
        let source =
            VaultCredentialSource::with_env_lookup(Arc::new(AsyncRwLock::new(vault)), |_| None);
        let catalog = default_catalog();

        assert_eq!(source.configured_providers(&catalog).await, vec![
            ProviderId::anthropic(),
            ProviderId::openai()
        ]);
    }

    #[tokio::test]
    async fn vault_only_ignores_env_lookup_values() {
        let env_dir = tempfile::tempdir().unwrap();
        let vault_only_dir = tempfile::tempdir().unwrap();
        let catalog = default_catalog();
        let env_backed = VaultCredentialSource::with_env_lookup(
            Arc::new(AsyncRwLock::new(
                Vault::load(env_dir.path().join("secrets.json")).unwrap(),
            )),
            |name| (name == "OPENAI_API_KEY").then(|| "env-openai-key".to_string()),
        );
        assert_eq!(env_backed.configured_providers(&catalog).await, vec![
            ProviderId::openai()
        ]);

        let vault_only = VaultCredentialSource::vault_only(Arc::new(AsyncRwLock::new(
            Vault::load(vault_only_dir.path().join("secrets.json")).unwrap(),
        )));

        assert!(
            vault_only.configured_providers(&catalog).await.is_empty(),
            "vault_only must not resolve env-backed provider keys"
        );
        let resolved = vault_only.resolve(&catalog).await.unwrap();
        assert!(resolved.credentials.is_empty());
        assert!(resolved.auth_issues.is_empty());
    }
}
