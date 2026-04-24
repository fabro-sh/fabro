use async_trait::async_trait;
use fabro_model::Provider;

use crate::{ApiCredential, ResolveError};

#[derive(Debug)]
pub struct ResolvedCredentials {
    pub credentials: Vec<ApiCredential>,
    pub auth_issues: Vec<(Provider, ResolveError)>,
}

#[async_trait]
pub trait CredentialSource: Send + Sync {
    async fn resolve(&self) -> anyhow::Result<ResolvedCredentials>;

    async fn resolve_providers(
        &self,
        providers: &[Provider],
    ) -> anyhow::Result<ResolvedCredentials> {
        let mut resolved = self.resolve().await?;
        resolved
            .credentials
            .retain(|credential| providers.contains(&credential.provider));
        resolved
            .auth_issues
            .retain(|(provider, _)| providers.contains(provider));
        Ok(resolved)
    }

    async fn configured_providers(&self) -> Vec<Provider>;
}
