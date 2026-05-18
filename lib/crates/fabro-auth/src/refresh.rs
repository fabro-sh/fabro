use crate::credential::{OAuthCredential, OAuthTokens, expires_at_from_now};

pub async fn refresh_oauth_credential(
    credential: &OAuthCredential,
) -> anyhow::Result<OAuthCredential> {
    let refresh_token = credential
        .tokens
        .refresh_token
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("refresh token missing"))?;
    let response = fabro_oauth::refresh_token(
        fabro_oauth::OAuthEndpoint {
            token_url: &credential.config.token_url,
            client_id: &credential.config.client_id,
        },
        refresh_token,
    )
    .await
    .map_err(anyhow::Error::msg)?;
    Ok(OAuthCredential {
        tokens:     OAuthTokens {
            access_token:  response.access_token,
            refresh_token: response
                .refresh_token
                .or_else(|| credential.tokens.refresh_token.clone()),
            expires_at:    expires_at_from_now(response.expires_in),
        },
        config:     credential.config.clone(),
        account_id: credential.account_id.clone(),
    })
}
