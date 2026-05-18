use fabro_types::SecretMetadata;
use fabro_vault::{Error as VaultError, SecretType, Vault};

use crate::credential::OAuthCredential;

#[derive(Debug, thiserror::Error)]
pub enum VaultLookupError {
    #[error("vault entry '{name}' has schema {actual:?}, expected {expected:?}")]
    SchemaMismatch {
        name:     String,
        expected: SecretType,
        actual:   SecretType,
    },
    #[error("vault entry '{name}' is not valid {expected:?} JSON: {source}")]
    DecodeFailed {
        name:     String,
        expected: SecretType,
        #[source]
        source:   serde_json::Error,
    },
}

pub fn vault_get_token(vault: &Vault, name: &str) -> Result<Option<String>, VaultLookupError> {
    let Some(entry) = vault.get_entry(name) else {
        return Ok(None);
    };
    if entry.secret_type != SecretType::Token {
        return Err(VaultLookupError::SchemaMismatch {
            name:     name.to_string(),
            expected: SecretType::Token,
            actual:   entry.secret_type,
        });
    }
    Ok(Some(entry.value.clone()))
}

pub fn vault_get_oauth(
    vault: &Vault,
    name: &str,
) -> Result<Option<OAuthCredential>, VaultLookupError> {
    let Some(entry) = vault.get_entry(name) else {
        return Ok(None);
    };
    if entry.secret_type != SecretType::Oauth {
        return Err(VaultLookupError::SchemaMismatch {
            name:     name.to_string(),
            expected: SecretType::Oauth,
            actual:   entry.secret_type,
        });
    }
    serde_json::from_str(&entry.value)
        .map(Some)
        .map_err(|source| VaultLookupError::DecodeFailed {
            name: name.to_string(),
            expected: SecretType::Oauth,
            source,
        })
}

pub fn vault_set_token(
    vault: &mut Vault,
    name: &str,
    value: &str,
) -> Result<SecretMetadata, VaultError> {
    vault.set(name, value, SecretType::Token, None)
}

pub fn vault_set_oauth(
    vault: &mut Vault,
    name: &str,
    credential: &OAuthCredential,
) -> Result<SecretMetadata, VaultError> {
    let json = serde_json::to_string(credential)?;
    vault.set(name, &json, SecretType::Oauth, None)
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;
    use crate::credential::{OAuthConfig, OAuthTokens};

    fn temp_vault() -> Vault {
        let dir = tempfile::tempdir().unwrap();
        Vault::load(dir.path().join("secrets.json")).unwrap()
    }

    fn fixture() -> OAuthCredential {
        OAuthCredential {
            tokens:     OAuthTokens {
                access_token:  "access".to_string(),
                refresh_token: Some("refresh".to_string()),
                expires_at:    Utc::now() + Duration::hours(1),
            },
            config:     OAuthConfig {
                auth_url:     "https://auth.openai.com".to_string(),
                token_url:    "https://auth.openai.com/oauth/token".to_string(),
                client_id:    "client".to_string(),
                scopes:       vec!["openid".to_string()],
                redirect_uri: None,
                use_pkce:     true,
            },
            account_id: None,
        }
    }

    #[test]
    fn vault_get_token_returns_none_when_absent() {
        let vault = temp_vault();
        assert!(
            vault_get_token(&vault, "ANTHROPIC_API_KEY")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn vault_get_token_returns_value_when_present() {
        let mut vault = temp_vault();
        vault_set_token(&mut vault, "ANTHROPIC_API_KEY", "sk-test").unwrap();
        assert_eq!(
            vault_get_token(&vault, "ANTHROPIC_API_KEY")
                .unwrap()
                .as_deref(),
            Some("sk-test"),
        );
    }

    #[test]
    fn vault_get_token_errors_on_oauth_entry() {
        let mut vault = temp_vault();
        vault_set_oauth(
            &mut vault,
            crate::OPENAI_CODEX_VAULT_SECRET_NAME,
            &fixture(),
        )
        .unwrap();
        let err = vault_get_token(&vault, crate::OPENAI_CODEX_VAULT_SECRET_NAME).unwrap_err();
        assert!(matches!(err, VaultLookupError::SchemaMismatch { .. }));
    }

    #[test]
    fn vault_get_oauth_round_trips() {
        let mut vault = temp_vault();
        let credential = fixture();
        vault_set_oauth(
            &mut vault,
            crate::OPENAI_CODEX_VAULT_SECRET_NAME,
            &credential,
        )
        .unwrap();
        assert_eq!(
            vault_get_oauth(&vault, crate::OPENAI_CODEX_VAULT_SECRET_NAME)
                .unwrap()
                .unwrap(),
            credential,
        );
    }
}
