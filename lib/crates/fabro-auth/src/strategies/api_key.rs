use async_trait::async_trait;
use fabro_model::Provider;

use crate::context::{AuthContextRequest, AuthContextResponse};
use crate::credential::{AuthCredential, AuthDetails};
use crate::strategy::AuthStrategy;

pub struct ApiKeyStrategy {
    provider: Provider,
}

impl ApiKeyStrategy {
    #[must_use]
    pub fn new(provider: Provider) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl AuthStrategy for ApiKeyStrategy {
    async fn init(&mut self) -> anyhow::Result<AuthContextRequest> {
        Ok(AuthContextRequest::ApiKey {
            provider: self.provider,
            env_var_names: self
                .provider
                .api_key_env_vars()
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
        })
    }

    async fn complete(&mut self, response: AuthContextResponse) -> anyhow::Result<AuthCredential> {
        match response {
            AuthContextResponse::ApiKey { key } => Ok(AuthCredential {
                provider: self.provider,
                details: AuthDetails::ApiKey { key },
            }),
            AuthContextResponse::DeviceCodeConfirmed => {
                Err(anyhow::anyhow!("expected API key response"))
            }
        }
    }
}
