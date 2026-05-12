use async_trait::async_trait;
use fabro_model::{Catalog, ProviderId};

use crate::{ApiCredential, ResolveError};

#[derive(Debug)]
pub struct ResolvedCredentials {
    pub credentials: Vec<ApiCredential>,
    pub auth_issues: Vec<(ProviderId, ResolveError)>,
}

#[async_trait]
pub trait CredentialSource: Send + Sync {
    async fn resolve(&self) -> anyhow::Result<ResolvedCredentials>;

    async fn configured_providers(&self) -> Vec<ProviderId>;

    async fn resolve_for_catalog(&self, catalog: &Catalog) -> anyhow::Result<ResolvedCredentials> {
        let _ = catalog;
        self.resolve().await
    }

    async fn configured_providers_for_catalog(&self, catalog: &Catalog) -> Vec<ProviderId> {
        let _ = catalog;
        self.configured_providers().await
    }
}
