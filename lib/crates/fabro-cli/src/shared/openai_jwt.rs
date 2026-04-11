use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;

pub(crate) const DEFAULT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub(crate) const DEFAULT_ISSUER: &str = "https://auth.openai.com";
pub(crate) const OAUTH_PORT: u16 = 1455;

#[derive(Deserialize)]
struct JwtPayload {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
    #[serde(default, rename = "https://api.openai.com/auth")]
    auth_claim:         Option<AuthClaim>,
    #[serde(default)]
    organizations:      Option<Vec<Organization>>,
}

#[derive(Deserialize)]
struct AuthClaim {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
}

#[derive(Deserialize)]
struct Organization {
    #[serde(default)]
    id: Option<String>,
}

fn parse_jwt_payload(token: &str) -> Option<JwtPayload> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload_bytes = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    serde_json::from_slice(&payload_bytes).ok()
}

pub(crate) fn extract_account_id(id_token: &str) -> Option<String> {
    let payload = parse_jwt_payload(id_token)?;
    payload
        .chatgpt_account_id
        .or_else(|| {
            payload
                .auth_claim
                .and_then(|claim| claim.chatgpt_account_id)
        })
        .or_else(|| {
            payload
                .organizations
                .and_then(|orgs| orgs.into_iter().next())
                .and_then(|org| org.id)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_jwt(claims: &serde_json::Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_string(claims).unwrap());
        format!("{header}.{payload}.signature")
    }

    #[test]
    fn parse_jwt_with_chatgpt_account_id() {
        let jwt = make_test_jwt(&serde_json::json!({
            "chatgpt_account_id": "acct_123"
        }));
        let payload = parse_jwt_payload(&jwt).unwrap();
        assert_eq!(payload.chatgpt_account_id.as_deref(), Some("acct_123"));
    }

    #[test]
    fn parse_jwt_with_nested_auth_claim() {
        let jwt = make_test_jwt(&serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_nested"
            }
        }));
        let payload = parse_jwt_payload(&jwt).unwrap();
        assert_eq!(
            payload
                .auth_claim
                .and_then(|claim| claim.chatgpt_account_id)
                .as_deref(),
            Some("acct_nested")
        );
    }

    #[test]
    fn parse_jwt_invalid_format() {
        assert!(parse_jwt_payload("not-a-jwt").is_none());
    }

    #[test]
    fn parse_jwt_invalid_base64() {
        assert!(parse_jwt_payload("header.!!!invalid!!!.sig").is_none());
    }

    #[test]
    fn extract_account_id_prefers_top_level() {
        let jwt = make_test_jwt(&serde_json::json!({
            "chatgpt_account_id": "top_level",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "nested"
            },
            "organizations": [{"id": "org"}]
        }));
        assert_eq!(extract_account_id(&jwt).as_deref(), Some("top_level"));
    }

    #[test]
    fn extract_account_id_falls_back_to_nested() {
        let jwt = make_test_jwt(&serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "nested"
            }
        }));
        assert_eq!(extract_account_id(&jwt).as_deref(), Some("nested"));
    }

    #[test]
    fn extract_account_id_falls_back_to_first_organization() {
        let jwt = make_test_jwt(&serde_json::json!({
            "organizations": [{"id": "org_456"}]
        }));
        assert_eq!(extract_account_id(&jwt).as_deref(), Some("org_456"));
    }

    #[test]
    fn extract_account_id_none_when_missing() {
        let jwt = make_test_jwt(&serde_json::json!({}));
        assert!(extract_account_id(&jwt).is_none());
    }
}
