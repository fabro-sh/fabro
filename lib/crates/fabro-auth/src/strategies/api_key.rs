use async_trait::async_trait;
use fabro_model::catalog::CatalogProvider;
use fabro_model::{CredentialRef, ProviderId};

use crate::context::{AuthContextRequest, AuthContextResponse};
use crate::strategy::{AuthStrategy, LoginResult};

pub struct ApiKeyStrategy {
    provider_id:   ProviderId,
    display_name:  String,
    env_var_names: Vec<String>,
    api_key_url:   Option<String>,
}

impl ApiKeyStrategy {
    #[must_use]
    pub fn new(provider: &CatalogProvider) -> Self {
        let env_var_names = provider
            .auth
            .as_ref()
            .map(|auth| {
                auth.credentials
                    .iter()
                    .filter_map(|credential_ref| match credential_ref {
                        CredentialRef::Env(name) => Some(name.clone()),
                        CredentialRef::Vault(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            provider_id: provider.id.clone(),
            display_name: provider.display_name.clone(),
            env_var_names,
            api_key_url: provider.api_key_url.clone(),
        }
    }
}

#[async_trait]
impl AuthStrategy for ApiKeyStrategy {
    async fn init(&mut self) -> anyhow::Result<AuthContextRequest> {
        Ok(AuthContextRequest::ApiKey {
            provider_id:   self.provider_id.clone(),
            display_name:  self.display_name.clone(),
            env_var_names: self.env_var_names.clone(),
            api_key_url:   self.api_key_url.clone(),
        })
    }

    async fn complete(&mut self, response: AuthContextResponse) -> anyhow::Result<LoginResult> {
        match response {
            AuthContextResponse::ApiKey { key } => Ok(LoginResult::ApiKey {
                provider: self.provider_id.clone(),
                key,
            }),
            AuthContextResponse::DeviceCodeConfirmed => {
                Err(anyhow::anyhow!("expected API key response"))
            }
        }
    }
}
