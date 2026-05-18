use chrono::{DateTime, Duration, Utc};
use fabro_redact::redact_string;
use serde::{Deserialize, Serialize};

/// JSON shape stored in the vault when `secret_type == Oauth`.
///
/// Provider context comes from the catalog and auth strategy at resolve time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthCredential {
    pub tokens:     OAuthTokens,
    pub config:     OAuthConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

impl OAuthCredential {
    #[must_use]
    pub fn needs_refresh(&self) -> bool {
        self.tokens.expires_at <= Utc::now() + Duration::minutes(5)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthTokens {
    pub access_token:  String,
    pub refresh_token: Option<String>,
    pub expires_at:    DateTime<Utc>,
}

pub(crate) fn expires_at_from_now(expires_in: Option<u64>) -> DateTime<Utc> {
    let seconds = i64::try_from(expires_in.unwrap_or(3600)).unwrap_or(i64::MAX);
    Utc::now() + Duration::seconds(seconds)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthConfig {
    pub auth_url:     String,
    pub token_url:    String,
    pub client_id:    String,
    pub scopes:       Vec<String>,
    pub redirect_uri: Option<String>,
    pub use_pkce:     bool,
}

#[derive(Clone, PartialEq, Eq)]
pub enum ApiKeyHeader {
    Bearer(String),
    Custom { name: String, value: String },
}

fn redact_for_debug(value: &str) -> String {
    let redacted = redact_string(value);
    if redacted == value && !value.is_empty() {
        "REDACTED".to_string()
    } else {
        redacted
    }
}

impl std::fmt::Debug for ApiKeyHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bearer(value) => f
                .debug_tuple("Bearer")
                .field(&redact_for_debug(value))
                .finish(),
            Self::Custom { name, value } => f
                .debug_struct("Custom")
                .field("name", name)
                .field("value", &redact_for_debug(value))
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(expires_at: DateTime<Utc>) -> OAuthCredential {
        OAuthCredential {
            tokens:     OAuthTokens {
                access_token: "access".to_string(),
                refresh_token: Some("refresh".to_string()),
                expires_at,
            },
            config:     OAuthConfig {
                auth_url:     "https://auth.openai.com".to_string(),
                token_url:    "https://auth.openai.com/oauth/token".to_string(),
                client_id:    "client".to_string(),
                scopes:       vec!["openid".to_string()],
                redirect_uri: Some("https://auth.openai.com/deviceauth/callback".to_string()),
                use_pkce:     true,
            },
            account_id: Some("acct_123".to_string()),
        }
    }

    #[test]
    fn round_trips_through_json() {
        let credential = fixture(Utc::now() + Duration::hours(1));
        let json = serde_json::to_string(&credential).unwrap();
        let parsed: OAuthCredential = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, credential);
    }

    #[test]
    fn needs_refresh_uses_five_minute_buffer() {
        assert!(fixture(Utc::now() + Duration::minutes(4)).needs_refresh());
        assert!(!fixture(Utc::now() + Duration::minutes(6)).needs_refresh());
    }

    #[test]
    fn api_key_header_debug_redacts_secret_values() {
        let header = ApiKeyHeader::Bearer("sk-test".to_string());
        let debug = format!("{header:?}");
        assert!(!debug.contains("sk-test"));
        assert!(debug.contains("REDACTED"));
    }
}
