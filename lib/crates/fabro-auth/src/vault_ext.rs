use fabro_model::Provider;
use fabro_vault::{SecretMetadata, SecretType, Vault};

use crate::credential::AuthCredential;

pub fn vault_set_credential(
    vault: &mut Vault,
    id: &str,
    credential: &AuthCredential,
) -> Result<SecretMetadata, fabro_vault::Error> {
    let json = serde_json::to_string(credential)?;
    vault.set(id, &json, SecretType::Credential, None)
}

#[must_use]
pub fn vault_get_credential(vault: &Vault, id: &str) -> Option<AuthCredential> {
    let entry = vault.get_entry(id)?;
    if entry.secret_type != SecretType::Credential {
        return None;
    }
    serde_json::from_str(&entry.value).ok()
}

#[must_use]
pub fn vault_credentials_for_provider(
    vault: &Vault,
    provider: Provider,
) -> Vec<(String, AuthCredential)> {
    vault
        .credential_entries()
        .into_iter()
        .filter_map(|(name, entry)| {
            serde_json::from_str::<AuthCredential>(&entry.value)
                .ok()
                .filter(|credential| credential.provider == provider)
                .map(|credential| (name.to_string(), credential))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;
    use crate::credential::{AuthDetails, OAuthConfig, OAuthTokens};

    fn oauth_credential() -> AuthCredential {
        AuthCredential {
            provider: Provider::OpenAi,
            details: AuthDetails::CodexOAuth {
                tokens: OAuthTokens {
                    access_token: "access".to_string(),
                    refresh_token: Some("refresh".to_string()),
                    expires_at: Utc::now() + Duration::hours(1),
                },
                config: OAuthConfig {
                    auth_url: "https://auth.openai.com".to_string(),
                    token_url: "https://auth.openai.com/oauth/token".to_string(),
                    client_id: "client".to_string(),
                    scopes: vec!["openid".to_string()],
                    redirect_uri: Some("https://auth.openai.com/deviceauth/callback".to_string()),
                    use_pkce: true,
                },
                account_id: Some("acct_123".to_string()),
            },
        }
    }

    #[test]
    fn vault_credential_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        let credential = oauth_credential();

        vault_set_credential(&mut vault, "openai_codex", &credential).unwrap();

        assert_eq!(
            vault_get_credential(&vault, "openai_codex").unwrap(),
            credential
        );
    }

    #[test]
    fn vault_credentials_for_provider_filters_by_provider() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_credential(&mut vault, "openai_codex", &oauth_credential()).unwrap();
        vault_set_credential(
            &mut vault,
            "anthropic",
            &AuthCredential {
                provider: Provider::Anthropic,
                details: AuthDetails::ApiKey {
                    key: "anthropic-key".to_string(),
                },
            },
        )
        .unwrap();

        let credentials = vault_credentials_for_provider(&vault, Provider::OpenAi);

        assert_eq!(credentials.len(), 1);
        assert_eq!(credentials[0].0, "openai_codex");
    }
}
