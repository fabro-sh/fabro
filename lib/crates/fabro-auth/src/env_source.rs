use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_model::Provider;

use crate::credential_source::{CredentialSource, ResolvedCredentials};
use crate::{ApiCredential, ApiKeyHeader, EnvLookup};

#[derive(Clone)]
pub struct EnvCredentialSource {
    env_lookup: EnvLookup,
}

impl EnvCredentialSource {
    #[must_use]
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

    fn credential_for(&self, provider: Provider) -> Option<ApiCredential> {
        match provider {
            Provider::Anthropic => self.lookup("ANTHROPIC_API_KEY").map(|key| ApiCredential {
                provider,
                auth_header: ApiKeyHeader::Custom {
                    name:  "x-api-key".to_string(),
                    value: key,
                },
                extra_headers: HashMap::new(),
                base_url: self.lookup("ANTHROPIC_BASE_URL"),
                codex_mode: false,
                org_id: None,
                project_id: None,
            }),
            Provider::OpenAi => self.lookup("OPENAI_API_KEY").map(|key| {
                let mut extra_headers = HashMap::new();
                let mut base_url = self.lookup("OPENAI_BASE_URL");
                let mut codex_mode = false;
                if let Some(account_id) = self.lookup("CHATGPT_ACCOUNT_ID") {
                    base_url = Some("https://chatgpt.com/backend-api/codex".to_string());
                    codex_mode = true;
                    extra_headers.insert("ChatGPT-Account-Id".to_string(), account_id);
                    extra_headers.insert("originator".to_string(), "fabro".to_string());
                }

                ApiCredential {
                    provider,
                    auth_header: ApiKeyHeader::Bearer(key),
                    extra_headers,
                    base_url,
                    codex_mode,
                    org_id: self.lookup("OPENAI_ORG_ID"),
                    project_id: self.lookup("OPENAI_PROJECT_ID"),
                }
            }),
            Provider::Gemini => self
                .lookup("GEMINI_API_KEY")
                .or_else(|| self.lookup("GOOGLE_API_KEY"))
                .map(|key| ApiCredential {
                    provider,
                    auth_header: ApiKeyHeader::Bearer(key),
                    extra_headers: HashMap::new(),
                    base_url: self.lookup("GEMINI_BASE_URL"),
                    codex_mode: false,
                    org_id: None,
                    project_id: None,
                }),
            Provider::Kimi => self.lookup("KIMI_API_KEY").map(|key| ApiCredential {
                provider,
                auth_header: ApiKeyHeader::Bearer(key),
                extra_headers: HashMap::new(),
                base_url: None,
                codex_mode: false,
                org_id: None,
                project_id: None,
            }),
            Provider::Zai => self.lookup("ZAI_API_KEY").map(|key| ApiCredential {
                provider,
                auth_header: ApiKeyHeader::Bearer(key),
                extra_headers: HashMap::new(),
                base_url: None,
                codex_mode: false,
                org_id: None,
                project_id: None,
            }),
            Provider::Minimax => self.lookup("MINIMAX_API_KEY").map(|key| ApiCredential {
                provider,
                auth_header: ApiKeyHeader::Bearer(key),
                extra_headers: HashMap::new(),
                base_url: None,
                codex_mode: false,
                org_id: None,
                project_id: None,
            }),
            Provider::Inception => self.lookup("INCEPTION_API_KEY").map(|key| ApiCredential {
                provider,
                auth_header: ApiKeyHeader::Bearer(key),
                extra_headers: HashMap::new(),
                base_url: None,
                codex_mode: false,
                org_id: None,
                project_id: None,
            }),
            Provider::OpenAiCompatible => None,
        }
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
        let credentials = Provider::ALL
            .iter()
            .copied()
            .filter_map(|provider| self.credential_for(provider))
            .collect();

        Ok(ResolvedCredentials {
            credentials,
            auth_issues: Vec::new(),
        })
    }

    async fn configured_providers(&self) -> Vec<Provider> {
        Provider::ALL
            .iter()
            .copied()
            .filter(|provider| {
                provider
                    .api_key_env_vars()
                    .iter()
                    .any(|env_var| self.lookup(env_var).is_some())
            })
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
            Provider::Anthropic
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

        assert_eq!(credential.provider, Provider::OpenAi);
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
}
