use async_trait::async_trait;
use fabro_model::catalog::CatalogProvider;
use fabro_model::{CredentialRef, ProviderId};

use crate::context::{AuthContextRequest, AuthContextResponse};
use crate::credential::{AuthCredential, AuthDetails};
use crate::strategy::AuthStrategy;

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
            .credentials
            .iter()
            .filter_map(|credential_ref| match credential_ref {
                CredentialRef::Env(name) => Some(name.clone()),
                CredentialRef::Credential(_) => None,
            })
            .collect();
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

    async fn complete(&mut self, response: AuthContextResponse) -> anyhow::Result<AuthCredential> {
        match response {
            AuthContextResponse::ApiKey { key } => Ok(AuthCredential {
                provider: self.provider_id.clone(),
                details:  AuthDetails::ApiKey { key },
            }),
            AuthContextResponse::DeviceCodeConfirmed => {
                Err(anyhow::anyhow!("expected API key response"))
            }
        }
    }
}
