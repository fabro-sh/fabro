use std::sync::Arc;

use async_trait::async_trait;
use fabro_model::catalog::CatalogProvider;
use fabro_model::{Catalog, CredentialRef, HeaderValueRef, Provider, ProviderId};
use fabro_static::EnvVars;

use crate::credential_source::{CredentialSource, ResolvedCredentials};
use crate::{ApiCredential, EnvLookup};

#[derive(Clone)]
pub struct EnvCredentialSource {
    env_lookup: EnvLookup,
}

impl EnvCredentialSource {
    #[must_use]
    #[expect(
        clippy::disallowed_methods,
        reason = "EnvCredentialSource is the provider API-key process-env facade."
    )]
    pub fn new() -> Self {
        Self::with_env_lookup(Arc::new(|name| std::env::var(name).ok()))
    }

    #[must_use]
    pub fn with_env_lookup(env_lookup: EnvLookup) -> Self {
        Self { env_lookup }
    }

    fn lookup(&self, name: &str) -> Option<String> {
        (self.env_lookup)(name)
    }

    fn credential_for(&self, provider: &CatalogProvider) -> Option<ApiCredential> {
        let key = provider.credentials.iter().find_map(|credential_ref| {
            let CredentialRef::Env(name) = credential_ref else {
                return None;
            };
            self.lookup(name)
        })?;

        let mut cred = ApiCredential::from_api_key(provider.id.clone(), key);
        cred.base_url = self
            .env_base_url(&provider.id)
            .or_else(|| provider.base_url.clone());
        cred.extra_headers = self.resolved_extra_headers(provider)?;
        if provider.id == Provider::OpenAi.id() {
            cred.org_id = self.lookup(EnvVars::OPENAI_ORG_ID);
            cred.project_id = self.lookup(EnvVars::OPENAI_PROJECT_ID);
            if let Some(account_id) = self.lookup(EnvVars::CHATGPT_ACCOUNT_ID) {
                cred.base_url = Some("https://chatgpt.com/backend-api/codex".to_string());
                cred.codex_mode = true;
                cred.extra_headers
                    .insert("ChatGPT-Account-Id".to_string(), account_id);
                cred.extra_headers
                    .insert("originator".to_string(), "fabro".to_string());
            }
        }
        Some(cred)
    }

    fn env_base_url(&self, provider: &ProviderId) -> Option<String> {
        match Provider::from_id(provider) {
            Some(Provider::Anthropic) => self.lookup(EnvVars::ANTHROPIC_BASE_URL),
            Some(Provider::OpenAi) => self.lookup(EnvVars::OPENAI_BASE_URL),
            Some(Provider::Gemini) => self.lookup(EnvVars::GEMINI_BASE_URL),
            Some(Provider::OpenAiCompatible) => self.lookup(EnvVars::OPENAI_COMPATIBLE_BASE_URL),
            Some(Provider::Kimi | Provider::Zai | Provider::Minimax | Provider::Inception)
            | None => None,
        }
    }

    fn resolved_extra_headers(
        &self,
        provider: &CatalogProvider,
    ) -> Option<std::collections::HashMap<String, String>> {
        provider
            .extra_headers
            .iter()
            .map(|(name, value_ref)| {
                let value = match value_ref {
                    HeaderValueRef::Literal(value) => Some(value.clone()),
                    HeaderValueRef::Env(name) => self.lookup(name),
                    HeaderValueRef::Credential(_) => None,
                }?;
                Some((name.clone(), value))
            })
            .collect()
    }
}

impl std::fmt::Debug for EnvCredentialSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvCredentialSource")
            .finish_non_exhaustive()
    }
}

impl Default for EnvCredentialSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CredentialSource for EnvCredentialSource {
    async fn resolve(&self) -> anyhow::Result<ResolvedCredentials> {
        let credentials = Catalog::builtin()
            .providers()
            .iter()
            .filter_map(|provider| self.credential_for(provider))
            .collect();

        Ok(ResolvedCredentials {
            credentials,
            auth_issues: Vec::new(),
        })
    }

    async fn configured_providers(&self) -> Vec<ProviderId> {
        Catalog::builtin()
            .providers()
            .iter()
            .filter(|provider| {
                provider
                    .credentials
                    .iter()
                    .any(|credential_ref| {
                        matches!(credential_ref, CredentialRef::Env(name) if self.lookup(name).is_some())
                    })
            })
            .map(|provider| provider.id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use fabro_model::Provider;

    use super::EnvCredentialSource;
    use crate::CredentialSource;

    fn test_source(entries: &[(&str, &str)]) -> EnvCredentialSource {
        let entries: HashMap<String, String> = entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        EnvCredentialSource::with_env_lookup(Arc::new(move |name| entries.get(name).cloned()))
    }

    #[tokio::test]
    async fn configured_providers_reads_injected_env() {
        let source = test_source(&[("ANTHROPIC_API_KEY", "anthropic-key")]);

        assert_eq!(source.configured_providers().await, vec![
            Provider::Anthropic.id()
        ]);
    }

    #[tokio::test]
    async fn resolve_returns_empty_when_no_keys_are_configured() {
        let source = test_source(&[]);

        let resolved = source.resolve().await.unwrap();

        assert!(resolved.credentials.is_empty());
        assert!(resolved.auth_issues.is_empty());
    }

    #[tokio::test]
    async fn resolve_builds_openai_codex_env_credential() {
        let source = test_source(&[
            ("OPENAI_API_KEY", "openai-key"),
            ("CHATGPT_ACCOUNT_ID", "acct_123"),
            ("OPENAI_PROJECT_ID", "project_123"),
        ]);

        let resolved = source.resolve().await.unwrap();
        let credential = resolved.credentials.first().unwrap();

        assert_eq!(credential.provider, Provider::OpenAi.id());
        assert!(credential.codex_mode);
        assert_eq!(
            credential.base_url.as_deref(),
            Some("https://chatgpt.com/backend-api/codex")
        );
        assert_eq!(
            credential.extra_headers.get("ChatGPT-Account-Id"),
            Some(&"acct_123".to_string())
        );
        assert_eq!(credential.project_id.as_deref(), Some("project_123"));
    }

    #[tokio::test]
    async fn resolve_uses_catalog_credentials_and_base_url_for_openai_compatible_providers() {
        let source = test_source(&[("KIMI_API_KEY", "kimi-key")]);

        let resolved = source.resolve().await.unwrap();
        let credential = resolved.credentials.first().unwrap();

        assert_eq!(credential.provider, Provider::Kimi.id());
        assert_eq!(
            credential.base_url.as_deref(),
            Some("https://api.moonshot.ai/v1")
        );
    }
}
