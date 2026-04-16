use crate::credential::{AuthCredential, AuthDetails, OAuthTokens, expires_at_from_now};

pub async fn refresh_oauth_credential(
    credential: &AuthCredential,
) -> anyhow::Result<AuthCredential> {
    match &credential.details {
        AuthDetails::ApiKey { .. } => Ok(credential.clone()),
        AuthDetails::CodexOAuth {
            tokens,
            config,
            account_id,
        } => {
            let refresh_token = tokens
                .refresh_token
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("refresh token missing"))?;
            let response = fabro_oauth::refresh_token(
                fabro_oauth::OAuthEndpoint {
                    token_url: &config.token_url,
                    client_id: &config.client_id,
                },
                refresh_token,
            )
            .await
            .map_err(anyhow::Error::msg)?;
            Ok(AuthCredential {
                provider: credential.provider,
                details: AuthDetails::CodexOAuth {
                    tokens: OAuthTokens {
                        access_token: response.access_token,
                        refresh_token: response
                            .refresh_token
                            .or_else(|| tokens.refresh_token.clone()),
                        expires_at: expires_at_from_now(response.expires_in),
                    },
                    config: config.clone(),
                    account_id: account_id.clone(),
                },
            })
        }
    }
}
