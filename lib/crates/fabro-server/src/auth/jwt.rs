use chrono::{Duration, Utc};
use fabro_types::{AuthMethod, IdpIdentity};
use jsonwebtoken::errors::{Error as JwtDecodeError, ErrorKind};
use jsonwebtoken::{Algorithm, Header, Validation, decode, decode_header, encode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::JwtSigningKey;

const ACCESS_TOKEN_AUDIENCE: &str = "fabro-cli";
const CLOCK_SKEW_SECS: u64 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JwtSubject {
    pub identity:    IdpIdentity,
    pub login:       String,
    pub name:        String,
    pub email:       String,
    pub avatar_url:  String,
    pub user_url:    String,
    pub auth_method: AuthMethod,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Claims {
    pub iss:         String,
    pub aud:         String,
    pub sub:         String,
    pub exp:         u64,
    pub iat:         u64,
    pub jti:         String,
    pub idp_issuer:  String,
    pub idp_subject: String,
    pub login:       String,
    pub name:        String,
    pub email:       String,
    #[serde(default)]
    pub avatar_url:  String,
    #[serde(default)]
    pub user_url:    String,
    pub auth_method: AuthMethod,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum JwtError {
    #[error("access token expired")]
    AccessTokenExpired,
    #[error("access token invalid")]
    AccessTokenInvalid,
}

pub(crate) fn issue(
    key: &JwtSigningKey,
    issuer: &str,
    subject: &JwtSubject,
    ttl: Duration,
) -> String {
    let now = Utc::now();
    let iat = now
        .timestamp()
        .try_into()
        .expect("current time should be positive");
    let exp = (now + ttl)
        .timestamp()
        .try_into()
        .expect("expiration time should be positive");
    let claims = Claims {
        iss: issuer.to_string(),
        aud: ACCESS_TOKEN_AUDIENCE.to_string(),
        sub: subject.identity.subject().to_string(),
        exp,
        iat,
        jti: Uuid::new_v4().to_string(),
        idp_issuer: subject.identity.issuer().to_string(),
        idp_subject: subject.identity.subject().to_string(),
        login: subject.login.clone(),
        name: subject.name.clone(),
        email: subject.email.clone(),
        avatar_url: subject.avatar_url.clone(),
        user_url: subject.user_url.clone(),
        auth_method: subject.auth_method,
    };

    encode(&Header::new(Algorithm::HS256), &claims, &key.encoding_key())
        .expect("HS256 access token should encode")
}

pub(crate) fn verify(
    key: &JwtSigningKey,
    expected_iss: &str,
    token: &str,
) -> Result<Claims, JwtError> {
    let header = decode_header(token).map_err(|_| JwtError::AccessTokenInvalid)?;
    if header.alg != Algorithm::HS256 || header.kid.is_some() {
        return Err(JwtError::AccessTokenInvalid);
    }

    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = CLOCK_SKEW_SECS;
    validation.validate_nbf = false;
    validation.set_audience(&[ACCESS_TOKEN_AUDIENCE]);
    validation.set_issuer(&[expected_iss]);

    let token_data = decode::<Claims>(token, &key.decoding_key(), &validation)
        .map_err(|err| map_decode_error(&err))?;

    let now: u64 = Utc::now()
        .timestamp()
        .try_into()
        .expect("current time should be positive");
    if token_data.claims.iat > now.saturating_add(CLOCK_SKEW_SECS) {
        return Err(JwtError::AccessTokenInvalid);
    }

    Ok(token_data.claims)
}

fn map_decode_error(err: &JwtDecodeError) -> JwtError {
    match err.kind() {
        ErrorKind::ExpiredSignature => JwtError::AccessTokenExpired,
        _ => JwtError::AccessTokenInvalid,
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use chrono::{Duration, Utc};
    use fabro_types::AuthMethod;
    use jsonwebtoken::{Algorithm, Header, encode};
    use serde::Serialize;
    use uuid::Uuid;

    use super::{Claims, JwtError, JwtSubject, issue, verify};
    use crate::auth::{self, JwtSigningKey};

    fn signing_key() -> JwtSigningKey {
        auth::derive_jwt_key(b"0123456789abcdef0123456789abcdef")
            .expect("jwt signing key should derive")
    }

    fn subject() -> JwtSubject {
        JwtSubject {
            identity:    fabro_types::IdpIdentity::new("https://github.com", "12345").unwrap(),
            login:       "octocat".to_string(),
            name:        "The Octocat".to_string(),
            email:       "octocat@example.com".to_string(),
            avatar_url:  "https://example.com/octocat.png".to_string(),
            user_url:    "https://github.com/octocat".to_string(),
            auth_method: AuthMethod::Github,
        }
    }

    fn claims_with_times(iat: i64, exp: i64) -> Claims {
        Claims {
            iss:         "https://fabro.example".to_string(),
            aud:         "fabro-cli".to_string(),
            sub:         "12345".to_string(),
            exp:         exp.try_into().unwrap(),
            iat:         iat.try_into().unwrap(),
            jti:         Uuid::new_v4().to_string(),
            idp_issuer:  "https://github.com".to_string(),
            idp_subject: "12345".to_string(),
            login:       "octocat".to_string(),
            name:        "The Octocat".to_string(),
            email:       "octocat@example.com".to_string(),
            avatar_url:  "https://example.com/octocat.png".to_string(),
            user_url:    "https://github.com/octocat".to_string(),
            auth_method: AuthMethod::Github,
        }
    }

    fn encode_claims(header: &Header, claims: &Claims) -> String {
        encode(header, claims, &signing_key().encoding_key()).expect("test token should encode")
    }

    fn forge_token(header: &serde_json::Value, claims: &Claims) -> String {
        forge_token_value(header, &serde_json::to_value(claims).unwrap())
    }

    fn forge_token_value(header: &serde_json::Value, claims: &serde_json::Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(header).unwrap());
        let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
        format!("{header}.{claims}.signature")
    }

    #[test]
    fn round_trips_claims() {
        let key = signing_key();
        let token = issue(
            &key,
            "https://fabro.example",
            &subject(),
            Duration::minutes(10),
        );

        let claims = verify(&key, "https://fabro.example", &token).unwrap();

        assert_eq!(claims.iss, "https://fabro.example");
        assert_eq!(claims.aud, "fabro-cli");
        assert_eq!(claims.idp_subject, "12345");
        assert_eq!(claims.avatar_url, "https://example.com/octocat.png");
        assert_eq!(claims.user_url, "https://github.com/octocat");
        assert_eq!(claims.auth_method, AuthMethod::Github);
        assert!(Uuid::parse_str(&claims.jti).is_ok());
    }

    #[test]
    fn legacy_tokens_without_profile_fields_default_to_empty_strings() {
        #[derive(Serialize)]
        struct LegacyClaims {
            iss:         String,
            aud:         String,
            sub:         String,
            exp:         u64,
            iat:         u64,
            jti:         String,
            idp_issuer:  String,
            idp_subject: String,
            login:       String,
            name:        String,
            email:       String,
            auth_method: AuthMethod,
        }

        let now = Utc::now().timestamp();
        let token = encode(
            &Header::new(Algorithm::HS256),
            &LegacyClaims {
                iss:         "https://fabro.example".to_string(),
                aud:         "fabro-cli".to_string(),
                sub:         "12345".to_string(),
                exp:         (now + 600).try_into().unwrap(),
                iat:         (now - 1).try_into().unwrap(),
                jti:         Uuid::new_v4().to_string(),
                idp_issuer:  "https://github.com".to_string(),
                idp_subject: "12345".to_string(),
                login:       "octocat".to_string(),
                name:        "The Octocat".to_string(),
                email:       "octocat@example.com".to_string(),
                auth_method: AuthMethod::Github,
            },
            &signing_key().encoding_key(),
        );

        let claims = verify(
            &signing_key(),
            "https://fabro.example",
            &token.expect("legacy token should encode"),
        )
        .unwrap();

        assert_eq!(claims.avatar_url, "");
        assert_eq!(claims.user_url, "");
    }

    #[test]
    fn rejects_alg_none_header() {
        let now = Utc::now().timestamp();
        let token = forge_token(
            &serde_json::json!({ "alg": "none", "typ": "JWT" }),
            &claims_with_times(now - 1, now + 600),
        );

        assert_eq!(
            verify(&signing_key(), "https://fabro.example", &token),
            Err(JwtError::AccessTokenInvalid)
        );
    }

    #[test]
    fn rejects_rs256_header() {
        let now = Utc::now().timestamp();
        let token = forge_token(
            &serde_json::json!({ "alg": "RS256", "typ": "JWT" }),
            &claims_with_times(now - 1, now + 600),
        );

        assert_eq!(
            verify(&signing_key(), "https://fabro.example", &token),
            Err(JwtError::AccessTokenInvalid)
        );
    }

    #[test]
    fn rejects_expired_tokens() {
        let now = Utc::now().timestamp();
        let token = encode_claims(
            &Header::new(Algorithm::HS256),
            &claims_with_times(now - 20, now - 10),
        );

        assert_eq!(
            verify(&signing_key(), "https://fabro.example", &token),
            Err(JwtError::AccessTokenExpired)
        );
    }

    #[test]
    fn allows_small_future_iat_skew() {
        let now = Utc::now().timestamp();
        let token = encode_claims(
            &Header::new(Algorithm::HS256),
            &claims_with_times(now + 3, now + 600),
        );

        assert!(verify(&signing_key(), "https://fabro.example", &token).is_ok());
    }

    #[test]
    fn rejects_large_future_iat_skew() {
        let now = Utc::now().timestamp();
        let token = encode_claims(
            &Header::new(Algorithm::HS256),
            &claims_with_times(now + 10, now + 600),
        );

        assert_eq!(
            verify(&signing_key(), "https://fabro.example", &token),
            Err(JwtError::AccessTokenInvalid)
        );
    }

    #[test]
    fn rejects_kid_header() {
        let now = Utc::now().timestamp();
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("kid-1".to_string());
        let token = encode_claims(&header, &claims_with_times(now - 1, now + 600));

        assert_eq!(
            verify(&signing_key(), "https://fabro.example", &token),
            Err(JwtError::AccessTokenInvalid)
        );
    }

    #[test]
    fn rejects_issuer_mismatch() {
        let token = issue(
            &signing_key(),
            "https://fabro.example",
            &subject(),
            Duration::minutes(10),
        );

        assert_eq!(
            verify(&signing_key(), "https://other.example", &token),
            Err(JwtError::AccessTokenInvalid)
        );
    }

    #[test]
    fn rejects_audience_mismatch() {
        let now = Utc::now().timestamp();
        let mut claims = claims_with_times(now - 1, now + 600);
        claims.aud = "not-fabro-cli".to_string();
        let token = encode_claims(&Header::new(Algorithm::HS256), &claims);

        assert_eq!(
            verify(&signing_key(), "https://fabro.example", &token),
            Err(JwtError::AccessTokenInvalid)
        );
    }

    #[test]
    fn rejects_tampered_signature() {
        let token = issue(
            &signing_key(),
            "https://fabro.example",
            &subject(),
            Duration::minutes(10),
        );
        let last = token.chars().last().unwrap();
        let replacement = if last == 'a' { 'b' } else { 'a' };
        let tampered = format!("{}{}", &token[..token.len() - 1], replacement);

        assert_eq!(
            verify(&signing_key(), "https://fabro.example", &tampered),
            Err(JwtError::AccessTokenInvalid)
        );
    }

    #[test]
    fn different_key_cannot_verify_token() {
        let token = issue(
            &signing_key(),
            "https://fabro.example",
            &subject(),
            Duration::minutes(10),
        );
        let other_key = auth::derive_jwt_key(b"abcdef0123456789abcdef0123456789")
            .expect("alternate key should derive");

        assert_eq!(
            verify(&other_key, "https://fabro.example", &token),
            Err(JwtError::AccessTokenInvalid)
        );
    }
}
