use chrono::{DateTime, Duration, Utc};
use fabro_model::Provider;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthCredential {
    pub provider: Provider,
    #[serde(flatten)]
    pub details: AuthDetails,
}

impl AuthCredential {
    #[must_use]
    pub fn needs_refresh(&self) -> bool {
        match &self.details {
            AuthDetails::ApiKey { .. } => false,
            AuthDetails::CodexOAuth { tokens, .. } => {
                tokens.expires_at <= Utc::now() + Duration::minutes(5)
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthDetails {
    ApiKey {
        key: String,
    },
    CodexOAuth {
        tokens: OAuthTokens,
        config: OAuthConfig,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account_id: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
}

pub(crate) fn expires_at_from_now(expires_in: Option<u64>) -> DateTime<Utc> {
    let seconds = i64::try_from(expires_in.unwrap_or(3600)).unwrap_or(i64::MAX);
    Utc::now() + Duration::seconds(seconds)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthConfig {
    pub auth_url: String,
    pub token_url: String,
    pub client_id: String,
    pub scopes: Vec<String>,
    pub redirect_uri: Option<String>,
    pub use_pkce: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiKeyHeader {
    Bearer(String),
    Custom { name: String, value: String },
}

pub fn credential_id_for(credential: &AuthCredential) -> Result<String, String> {
    match (&credential.provider, &credential.details) {
        (Provider::OpenAi, AuthDetails::ApiKey { .. }) => Ok("openai".to_string()),
        (Provider::OpenAi, AuthDetails::CodexOAuth { .. }) => Ok("openai_codex".to_string()),
        (_, AuthDetails::CodexOAuth { .. }) => Err(format!(
            "codex_oauth credentials are only valid for OpenAI, got {}",
            credential.provider
        )),
        (provider, AuthDetails::ApiKey { .. }) => Ok(provider.as_str().to_string()),
    }
}

pub fn parse_credential_secret(name: &str, value: &str) -> Result<AuthCredential, String> {
    let credential: AuthCredential =
        serde_json::from_str(value).map_err(|err| format!("invalid credential JSON: {err}"))?;
    let expected_name = credential_id_for(&credential)?;
    if name != expected_name {
        return Err(format!(
            "credential ID must be '{expected_name}' for this credential, got '{name}'"
        ));
    }
    Ok(credential)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oauth_credential(expires_at: DateTime<Utc>) -> AuthCredential {
        AuthCredential {
            provider: Provider::OpenAi,
            details: AuthDetails::CodexOAuth {
                tokens: OAuthTokens {
                    access_token: "access".to_string(),
                    refresh_token: Some("refresh".to_string()),
                    expires_at,
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
    fn auth_credential_round_trips_through_json() {
        let credential = oauth_credential(Utc::now() + Duration::hours(1));
        let json = serde_json::to_string(&credential).unwrap();
        let parsed: AuthCredential = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, credential);
    }

    #[test]
    fn needs_refresh_uses_five_minute_buffer() {
        assert!(oauth_credential(Utc::now() + Duration::minutes(4)).needs_refresh());
        assert!(!oauth_credential(Utc::now() + Duration::minutes(6)).needs_refresh());
    }

    #[test]
    fn credential_id_for_openai_codex_oauth() {
        let credential = oauth_credential(Utc::now() + Duration::hours(1));
        assert_eq!(credential_id_for(&credential).unwrap(), "openai_codex");
    }

    #[test]
    fn credential_id_for_openai_api_key() {
        let credential = AuthCredential {
            provider: Provider::OpenAi,
            details: AuthDetails::ApiKey {
                key: "sk-test".to_string(),
            },
        };
        assert_eq!(credential_id_for(&credential).unwrap(), "openai");
    }

    #[test]
    fn credential_id_for_non_openai_codex_oauth_errors() {
        let mut credential = oauth_credential(Utc::now() + Duration::hours(1));
        credential.provider = Provider::Anthropic;
        assert!(credential_id_for(&credential).is_err());
    }

    #[test]
    fn parse_credential_secret_validates_name_and_json() {
        let credential = oauth_credential(Utc::now() + Duration::hours(1));
        let json = serde_json::to_string(&credential).unwrap();

        assert!(parse_credential_secret("openai_codex", &json).is_ok());
        assert!(parse_credential_secret("openai", &json).is_err());
        assert!(parse_credential_secret("openai_codex", "{").is_err());
    }
}
