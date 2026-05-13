use std::sync::Arc;

use async_trait::async_trait;
use fabro_model::catalog::CatalogProvider;
use fabro_model::{
    Catalog, CredentialRef, HeaderValueRef, Provider, ProviderId, adapter, bootstrap_catalog,
};
use fabro_static::EnvVars;

use crate::credential_source::{CredentialSource, ResolvedCredentials};
use crate::{ApiCredential, EnvLookup, ResolveError, build_api_key_header};

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

    fn credential_for(
        &self,
        provider: &CatalogProvider,
    ) -> Result<Option<ApiCredential>, ResolveError> {
        let key = provider.credentials.iter().find_map(|credential_ref| {
            let CredentialRef::Env(name) = credential_ref else {
                return None;
            };
            self.lookup(name)
        });

        if key.is_none() && provider.credentials.is_empty() && provider.extra_headers.is_empty() {
            return Ok(None);
        }

        let extra_headers = self.resolved_extra_headers(provider)?;

        if key.is_none() && (!provider.credentials.is_empty() || extra_headers.is_empty()) {
            return Ok(None);
        }

        let auth_header = key.map(|key| {
            let policy = adapter::get(&provider.adapter)
                .map_or(fabro_model::ApiKeyHeaderPolicy::Bearer, |adapter| {
                    adapter.api_key_header
                });
            build_api_key_header(policy, key)
        });

        let mut cred = ApiCredential {
            provider: provider.id.clone(),
            auth_header,
            extra_headers,
            base_url: None,
            codex_mode: false,
            org_id: None,
            project_id: None,
        };
        cred.base_url = self
            .env_base_url(&provider.id)
            .or_else(|| provider.base_url.clone());
        if provider.id == Provider::OpenAi.id() && cred.auth_header.is_some() {
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
        Ok(Some(cred))
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
    ) -> Result<std::collections::HashMap<String, String>, ResolveError> {
        provider
            .extra_headers
            .iter()
            .map(|(name, value_ref)| {
                let value = match value_ref {
                    HeaderValueRef::Literal(value) => Some(value.clone()),
                    HeaderValueRef::Env(name) => self.lookup(name),
                    HeaderValueRef::Credential(_) => None,
                }
                .ok_or_else(|| ResolveError::NotConfigured(provider.id.clone()))?;
                Ok((name.clone(), value))
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
        self.resolve_for_catalog(bootstrap_catalog::catalog()).await
    }

    async fn resolve_for_catalog(&self, catalog: &Catalog) -> anyhow::Result<ResolvedCredentials> {
        let mut credentials = Vec::new();
        let mut auth_issues = Vec::new();

        for provider in catalog.providers() {
            match self.credential_for(provider) {
                Ok(Some(credential)) => credentials.push(credential),
                Ok(None) => {}
                Err(ResolveError::NotConfigured(_)) if !provider.credentials.is_empty() => {}
                Err(err) => auth_issues.push((provider.id.clone(), err)),
            }
        }

        Ok(ResolvedCredentials {
            credentials,
            auth_issues,
        })
    }

    async fn configured_providers(&self) -> Vec<ProviderId> {
        self.configured_providers_for_catalog(bootstrap_catalog::catalog())
            .await
    }

    async fn configured_providers_for_catalog(&self, catalog: &Catalog) -> Vec<ProviderId> {
        catalog
            .providers()
            .iter()
            .filter(|provider| {
                provider
                    .credentials
                    .iter()
                    .any(|credential_ref| {
                        matches!(credential_ref, CredentialRef::Env(name) if self.lookup(name).is_some())
                    })
                    || (!provider.extra_headers.is_empty()
                        && provider.credentials.is_empty()
                        && self.resolved_extra_headers(provider).is_ok())
            })
            .map(|provider| provider.id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use fabro_model::catalog::LlmCatalogSettings;
    use fabro_model::{Catalog, Provider, ProviderId};

    use super::EnvCredentialSource;
    use crate::CredentialSource;

    fn test_source(entries: &[(&str, &str)]) -> EnvCredentialSource {
        let entries: HashMap<String, String> = entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        EnvCredentialSource::with_env_lookup(Arc::new(move |name| entries.get(name).cloned()))
    }

    fn catalog_with(overrides: &str) -> Catalog {
        let settings: LlmCatalogSettings = toml::from_str(overrides).unwrap();
        Catalog::from_builtin_with_overrides(&settings).unwrap()
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

    #[tokio::test]
    async fn resolve_for_catalog_registers_custom_env_backed_provider() {
        let catalog = catalog_with(
            r#"
[providers.venice]
display_name = "Venice"
adapter = "openai_compatible"
base_url = "https://api.venice.ai/api/v1"
credentials = ["env:VENICE_API_KEY"]

[models."venice-large"]
provider = "venice"
display_name = "Venice Large"
family = "venice"
default = true

[models."venice-large".limits]
context_window = 128000

[models."venice-large".features]
tools = true
vision = false
reasoning = false
effort = false
"#,
        );
        let source = test_source(&[("VENICE_API_KEY", "venice-key")]);

        let resolved = source.resolve_for_catalog(&catalog).await.unwrap();
        let credential = resolved
            .credentials
            .iter()
            .find(|credential| credential.provider == ProviderId::new("venice"))
            .expect("custom provider should resolve from the supplied catalog");

        assert_eq!(
            credential.auth_header.as_ref().unwrap(),
            &crate::ApiKeyHeader::Bearer("venice-key".to_string(),)
        );
        assert_eq!(
            credential.base_url.as_deref(),
            Some("https://api.venice.ai/api/v1")
        );
    }

    #[tokio::test]
    async fn resolve_for_catalog_registers_header_only_provider() {
        let catalog = catalog_with(
            r#"
[providers.portkey]
display_name = "Portkey Bedrock"
adapter = "anthropic"
base_url = "https://api.portkey.ai/v1"

[providers.portkey.extra_headers]
x-portkey-api-key = { env = "PORTKEY_API_KEY" }
x-portkey-provider = { literal = "@bedrock-prod" }

[models."portkey-claude"]
provider = "portkey"
display_name = "Portkey Claude"
family = "claude"
default = true

[models."portkey-claude".limits]
context_window = 200000

[models."portkey-claude".features]
tools = true
vision = true
reasoning = true
effort = true
"#,
        );
        let source = test_source(&[("PORTKEY_API_KEY", "pk-live")]);

        let resolved = source.resolve_for_catalog(&catalog).await.unwrap();
        let credential = resolved
            .credentials
            .iter()
            .find(|credential| credential.provider == ProviderId::new("portkey"))
            .expect("header-only provider should register when all headers resolve");

        assert!(credential.auth_header.is_none());
        assert_eq!(
            credential.extra_headers.get("x-portkey-api-key"),
            Some(&"pk-live".to_string())
        );
        assert_eq!(
            credential.extra_headers.get("x-portkey-provider"),
            Some(&"@bedrock-prod".to_string())
        );
    }

    #[tokio::test]
    async fn resolve_for_catalog_reports_missing_required_header() {
        let catalog = catalog_with(
            r#"
[providers.portkey]
display_name = "Portkey Bedrock"
adapter = "anthropic"
base_url = "https://api.portkey.ai/v1"

[providers.portkey.extra_headers]
x-portkey-api-key = { env = "PORTKEY_API_KEY" }

[models."portkey-claude"]
provider = "portkey"
display_name = "Portkey Claude"
family = "claude"
default = true

[models."portkey-claude".limits]
context_window = 200000

[models."portkey-claude".features]
tools = true
vision = true
reasoning = true
effort = true
"#,
        );
        let source = test_source(&[]);

        let resolved = source.resolve_for_catalog(&catalog).await.unwrap();

        assert!(
            !resolved
                .credentials
                .iter()
                .any(|credential| credential.provider == ProviderId::new("portkey"))
        );
        assert!(
            resolved
                .auth_issues
                .iter()
                .any(|(provider, issue)| provider == &ProviderId::new("portkey")
                    && matches!(issue, crate::ResolveError::NotConfigured(_)))
        );
    }
}
