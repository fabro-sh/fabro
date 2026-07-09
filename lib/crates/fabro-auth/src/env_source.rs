use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_model::catalog::CatalogProvider;
use fabro_model::{Catalog, CredentialRef, ProviderId};
use fabro_static::EnvVars;
use fabro_types::settings::ResolveCtx;

use crate::credential_source::{CredentialSource, ResolvedCredentials};
use crate::resolve::{apply_openai_api_env_context, apply_openai_codex_api_context};
use crate::{ApiCredential, EnvLookup, ResolveError, build_api_key_header, resolve};

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
        let (auth_header, extra_headers) = match &provider.auth {
            Some(auth) => {
                let Some(key) = auth.credentials.iter().find_map(|credential_ref| {
                    let CredentialRef::Env(name) = credential_ref else {
                        return None;
                    };
                    self.lookup(name)
                }) else {
                    return Ok(None);
                };
                (
                    Some(build_api_key_header(auth.header.clone(), key)),
                    self.resolved_extra_headers(provider)?,
                )
            }
            None => (None, self.resolved_extra_headers(provider)?),
        };

        let mut cred = ApiCredential {
            provider: provider.id.clone(),
            auth_header,
            extra_headers,
            base_url: provider.base_url.clone(),
            codex_mode: false,
            org_id: None,
            project_id: None,
        };
        if provider.id == ProviderId::openai() && cred.auth_header.is_some() {
            if let Some(account_id) = self.lookup(EnvVars::CHATGPT_ACCOUNT_ID) {
                apply_openai_codex_api_context(&mut cred, Some(&account_id), &*self.env_lookup);
            } else {
                apply_openai_api_env_context(&mut cred, &*self.env_lookup);
            }
        }
        Ok(Some(cred))
    }

    fn resolved_extra_headers(
        &self,
        provider: &CatalogProvider,
    ) -> Result<HashMap<String, String>, ResolveError> {
        let mut ctx = ResolveCtx::new().with_env(|env_name| self.lookup(env_name));
        resolve::resolve_extra_headers(&provider.id, &provider.extra_headers, &mut ctx)
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
    async fn resolve(&self, catalog: &Catalog) -> anyhow::Result<ResolvedCredentials> {
        let mut credentials = Vec::new();
        let mut auth_issues = Vec::new();

        for provider in catalog.providers() {
            match self.credential_for(provider) {
                Ok(Some(credential)) => credentials.push(credential),
                Ok(None) => {}
                Err(ResolveError::NotConfigured(_) | ResolveError::Interpolation { .. })
                    if provider.auth.is_some() => {}
                Err(err) => auth_issues.push((provider.id.clone(), err)),
            }
        }

        Ok(ResolvedCredentials {
            credentials,
            auth_issues,
        })
    }

    async fn configured_providers(&self, catalog: &Catalog) -> Vec<ProviderId> {
        catalog
            .providers()
            .iter()
            .filter(|provider| match &provider.auth {
                Some(auth) => auth.credentials.iter().any(|credential_ref| {
                    matches!(credential_ref, CredentialRef::Env(name) if self.lookup(name).is_some())
                }),
                None => self.resolved_extra_headers(provider).is_ok(),
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
    use fabro_model::{Catalog, ProviderId};

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

    fn default_catalog() -> Catalog {
        catalog_with("")
    }

    /// A no-auth portkey provider whose only variation is its `extra_headers`
    /// TOML lines.
    fn portkey_catalog(extra_headers: &str) -> Catalog {
        catalog_with(&format!(
            r#"
[providers.portkey]
display_name = "Portkey Bedrock"
adapter = "anthropic"
agent_profile = "anthropic"
base_url = "https://api.portkey.ai/v1"

[providers.portkey.extra_headers]
{extra_headers}

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
reasoning_effort = "levels"
"#
        ))
    }

    #[tokio::test]
    async fn configured_providers_reads_injected_env() {
        let source = test_source(&[("ANTHROPIC_API_KEY", "anthropic-key")]);
        let catalog = default_catalog();

        assert_eq!(source.configured_providers(&catalog).await, vec![
            ProviderId::anthropic()
        ]);
    }

    #[tokio::test]
    async fn resolve_returns_empty_when_no_keys_are_configured() {
        let source = test_source(&[]);
        let catalog = default_catalog();

        let resolved = source.resolve(&catalog).await.unwrap();

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
        let catalog = default_catalog();

        let resolved = source.resolve(&catalog).await.unwrap();
        let credential = resolved.credentials.first().unwrap();

        assert_eq!(credential.provider, ProviderId::openai());
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
        let catalog = default_catalog();

        let resolved = source.resolve(&catalog).await.unwrap();
        let credential = resolved.credentials.first().unwrap();

        assert_eq!(credential.provider, ProviderId::new("kimi"));
        assert_eq!(
            credential.base_url.as_deref(),
            Some("https://api.moonshot.ai/v1")
        );
    }

    #[tokio::test]
    async fn resolve_registers_custom_env_backed_provider() {
        let catalog = catalog_with(
            r#"
[providers.acme]
display_name = "Acme"
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "https://api.acme.test/v1"

[providers.acme.auth]
credentials = ["env:ACME_API_KEY"]

[models."acme-large"]
provider = "acme"
display_name = "Acme Large"
family = "acme"
default = true

[models."acme-large".limits]
context_window = 128000

[models."acme-large".features]
tools = true
vision = false
reasoning = false
"#,
        );
        let source = test_source(&[("ACME_API_KEY", "acme-key")]);

        let resolved = source.resolve(&catalog).await.unwrap();
        let credential = resolved
            .credentials
            .iter()
            .find(|credential| credential.provider == ProviderId::new("acme"))
            .expect("custom provider should resolve from the supplied catalog");

        assert_eq!(
            credential.auth_header.as_ref().unwrap(),
            &crate::ApiKeyHeader::Bearer("acme-key".to_string(),)
        );
        assert_eq!(
            credential.base_url.as_deref(),
            Some("https://api.acme.test/v1")
        );
    }

    #[tokio::test]
    async fn env_source_resolves_literal_and_env_header_tokens() {
        let catalog = portkey_catalog(
            r#"
x-portkey-api-key = "{{ env.PORTKEY_API_KEY }}"
x-portkey-provider = "@bedrock-prod"
"#,
        );
        let source = test_source(&[("PORTKEY_API_KEY", "pk-live")]);

        let resolved = source.resolve(&catalog).await.unwrap();
        let credential = resolved
            .credentials
            .iter()
            .find(|credential| credential.provider == ProviderId::new("portkey"))
            .expect("no-auth provider should register when extra headers resolve");

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
    async fn env_source_secrets_header_token_is_unavailable() {
        let catalog = portkey_catalog(r#"x-team-secret = "{{ secrets.gateway_team_secret }}""#);
        let source = test_source(&[]);

        let resolved = source.resolve(&catalog).await.unwrap();

        assert!(
            !resolved
                .credentials
                .iter()
                .any(|credential| credential.provider == ProviderId::new("portkey"))
        );
        let (_, issue) = resolved
            .auth_issues
            .iter()
            .find(|(provider, _)| provider == &ProviderId::new("portkey"))
            .expect("secrets token should surface as an auth issue");
        assert!(matches!(
            issue,
            crate::ResolveError::Interpolation { provider, .. }
                if provider == &ProviderId::new("portkey")
        ));
        assert!(issue.to_string().contains("gateway_team_secret"));
    }

    #[tokio::test]
    async fn env_source_reports_missing_env_header_for_no_auth_provider() {
        let catalog = portkey_catalog(r#"x-portkey-api-key = "{{ env.PORTKEY_API_KEY }}""#);
        let source = test_source(&[]);

        let resolved = source.resolve(&catalog).await.unwrap();

        assert!(
            !resolved
                .credentials
                .iter()
                .any(|credential| credential.provider == ProviderId::new("portkey"))
        );
        let (_, issue) = resolved
            .auth_issues
            .iter()
            .find(|(provider, _)| provider == &ProviderId::new("portkey"))
            .expect("missing env header should surface as an auth issue");
        assert!(matches!(issue, crate::ResolveError::Interpolation { .. }));
        assert!(issue.to_string().contains("PORTKEY_API_KEY"));
    }
}
