use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_types::RunId;
use lithos_llm::catalog::CatalogProvider;
use lithos_llm::credentials::{
    CredentialError, CredentialHeader, CredentialProvider, Credentials, SecretValue,
};

/// Decorates another [`CredentialProvider`] by appending fixed extra headers
/// to every HTTP credential it resolves.
///
/// Headers already present on a credential (for example from explicit
/// provider configuration) or on the provider catalog are left untouched.
/// AWS-signed credentials carry no header list and pass through unchanged.
pub struct ExtraHeadersCredentialSource {
    inner:   Arc<dyn CredentialProvider>,
    headers: HashMap<String, String>,
}

impl ExtraHeadersCredentialSource {
    /// Tag HTTP model calls with their owning run, including retries and
    /// resume. Explicit provider headers still take precedence; AWS
    /// credentials are unchanged.
    #[must_use]
    pub fn for_run(inner: Arc<dyn CredentialProvider>, run_id: RunId) -> Self {
        Self::new(
            inner,
            HashMap::from([("x-session-id".to_string(), run_id.to_string())]),
        )
    }

    #[must_use]
    pub fn new(inner: Arc<dyn CredentialProvider>, headers: HashMap<String, String>) -> Self {
        Self { inner, headers }
    }
}

#[async_trait]
impl CredentialProvider for ExtraHeadersCredentialSource {
    async fn credentials(
        &self,
        provider: &CatalogProvider,
    ) -> Result<Credentials, CredentialError> {
        let mut credentials = self.inner.credentials(provider).await?;
        if let Credentials::Http(http) = &mut credentials {
            for (name, value) in &self.headers {
                if provider
                    .default_headers()
                    .keys()
                    .any(|existing| existing.eq_ignore_ascii_case(name))
                    || http
                        .extra_headers
                        .iter()
                        .any(|existing| existing.name.eq_ignore_ascii_case(name))
                {
                    continue;
                }
                http.extra_headers.push(CredentialHeader::new(
                    name.clone(),
                    SecretValue::new(value.clone()),
                ));
            }
        }
        Ok(credentials)
    }

    async fn is_configured(&self, provider: &CatalogProvider) -> bool {
        self.inner.is_configured(provider).await
    }
}

#[cfg(test)]
mod tests {
    use lithos_llm::credentials::HttpAuthentication;

    use super::*;
    use crate::test_support::test_catalog;

    struct StubSource {
        configured:      bool,
        existing_header: Option<(String, String)>,
    }

    #[async_trait]
    impl CredentialProvider for StubSource {
        async fn credentials(
            &self,
            provider: &CatalogProvider,
        ) -> Result<Credentials, CredentialError> {
            if provider.id().as_str() == "bedrock" {
                return Ok(Credentials::AwsDefaultChain { region: None });
            }
            let mut credentials = Credentials::bearer(SecretValue::new("key"));
            if let (Credentials::Http(http), Some((name, value))) =
                (&mut credentials, &self.existing_header)
            {
                http.extra_headers.push(CredentialHeader::new(
                    name.clone(),
                    SecretValue::new(value.clone()),
                ));
            }
            Ok(credentials)
        }

        async fn is_configured(&self, _provider: &CatalogProvider) -> bool {
            self.configured
        }
    }

    fn header<'a>(credentials: &'a Credentials, name: &str) -> Option<&'a str> {
        match credentials {
            Credentials::Http(http) => http
                .extra_headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case(name))
                .map(|header| header.value.expose_secret()),
            _ => None,
        }
    }

    #[tokio::test]
    async fn appends_headers_to_http_credentials_only() {
        let catalog = test_catalog();
        let source = ExtraHeadersCredentialSource::new(
            Arc::new(StubSource {
                configured:      false,
                existing_header: None,
            }),
            HashMap::from([("x-session-id".to_string(), "run-123".to_string())]),
        );
        let openai = source
            .credentials(catalog.provider("openai").unwrap())
            .await
            .unwrap();
        assert!(matches!(
            &openai,
            Credentials::Http(http) if matches!(http.auth, HttpAuthentication::Bearer(_))
        ));
        assert_eq!(header(&openai, "x-session-id"), Some("run-123"));
        let bedrock = source
            .credentials(catalog.provider("bedrock").unwrap())
            .await
            .unwrap();
        assert!(matches!(bedrock, Credentials::AwsDefaultChain { .. }));
    }

    #[tokio::test]
    async fn preserves_case_insensitive_headers_already_set() {
        let catalog = test_catalog();
        let source = ExtraHeadersCredentialSource::new(
            Arc::new(StubSource {
                configured:      true,
                existing_header: Some(("X-Session-Id".to_string(), "configured".to_string())),
            }),
            HashMap::from([("x-session-id".to_string(), "run-123".to_string())]),
        );
        let credentials = source
            .credentials(catalog.provider("openai").unwrap())
            .await
            .unwrap();
        assert_eq!(header(&credentials, "x-session-id"), Some("configured"));
        assert!(
            source
                .is_configured(catalog.provider("openai").unwrap())
                .await
        );
    }
    #[tokio::test]
    async fn run_headers_preserve_aws_and_bedrock_credentials() {
        use lithos_llm::credentials::StaticCredentials;

        let catalog = test_catalog();
        let run_id = RunId::new();
        for credentials in [
            Credentials::AwsDefaultChain {
                region: Some("us-west-2".to_string()),
            },
            Credentials::BedrockBearer(SecretValue::new("bedrock-key")),
        ] {
            let inner = StaticCredentials::new().with("bedrock", credentials.clone());
            let source = ExtraHeadersCredentialSource::for_run(Arc::new(inner), run_id);
            assert_eq!(
                source
                    .credentials(catalog.provider("bedrock").unwrap())
                    .await
                    .unwrap(),
                credentials
            );
        }
    }
    #[tokio::test]
    async fn run_headers_do_not_cache_credentials_or_hide_resolution_errors() {
        use fabro_types::SecretType;

        use crate::VaultCredentialSource;
        use crate::test_support::empty_vault;

        let catalog = test_catalog();
        let provider = catalog.provider("openai").unwrap();
        let vault = empty_vault();
        let inner: Arc<dyn CredentialProvider> =
            Arc::new(VaultCredentialSource::vault_only(Arc::clone(&vault)));
        let source = ExtraHeadersCredentialSource::for_run(Arc::clone(&inner), RunId::new());
        assert!(!source.is_configured(provider).await);
        let expected = inner.credentials(provider).await.unwrap_err();
        let actual = source.credentials(provider).await.unwrap_err();
        assert_eq!(
            std::mem::discriminant(&actual),
            std::mem::discriminant(&expected)
        );
        assert_eq!(actual.to_string(), expected.to_string());
        for key in ["initial-key", "refreshed-key"] {
            vault
                .write()
                .await
                .set("OPENAI_API_KEY", key, SecretType::Token, None)
                .unwrap();
            let Credentials::Http(http) = source.credentials(provider).await.unwrap() else {
                panic!("HTTP credentials")
            };
            let HttpAuthentication::Bearer(actual) = http.auth else {
                panic!("bearer authentication")
            };
            assert_eq!(actual.expose_secret(), key);
        }
        assert!(source.is_configured(provider).await);
    }
}
