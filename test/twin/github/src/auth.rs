use axum::http::HeaderMap;

use crate::state::{AppState, PermissionLevel, TokenInfo, TokenPermission};

/// Verify a GitHub App JWT (RS256) and return the `iss` claim (app_id).
///
/// Accepts a **public** key PEM. The caller obtains this from `RegisteredApp::public_key_pem`,
/// which is derived from the private key during `AppState::register_app`.
pub fn verify_app_jwt(jwt: &str, public_key_pem: &str) -> Result<String, String> {
    use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Claims {
        iss: String,
    }

    let key = DecodingKey::from_rsa_pem(public_key_pem.as_bytes())
        .map_err(|e| format!("Invalid RSA public key: {e}"))?;

    let mut validation = Validation::new(Algorithm::RS256);
    validation.validate_exp = true;
    validation.set_required_spec_claims(&["iss", "iat", "exp"]);

    let data = decode::<Claims>(jwt, &key, &validation)
        .map_err(|e| format!("JWT verification failed: {e}"))?;

    Ok(data.claims.iss)
}

/// Extract Bearer token from Authorization header.
pub fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BearerTokenError {
    Missing,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallationTokenAccessError {
    RepoNotAccessible,
    PermissionDenied,
}

pub enum GraphqlActor {
    InstallationToken(TokenInfo),
    AppJwt,
}

pub fn verify_any_app_jwt(state: &AppState, jwt: &str) -> bool {
    state
        .apps
        .values()
        .any(|app| verify_app_jwt(jwt, &app.public_key_pem).is_ok())
}

pub fn authorize_installation_token(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<TokenInfo, BearerTokenError> {
    let token = extract_bearer_token(headers).ok_or(BearerTokenError::Missing)?;
    state
        .validate_token(&token)
        .cloned()
        .ok_or(BearerTokenError::Invalid)
}

pub fn authorize_graphql_actor(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<GraphqlActor, BearerTokenError> {
    let token = extract_bearer_token(headers).ok_or(BearerTokenError::Missing)?;
    if let Some(token_info) = state.validate_token(&token) {
        return Ok(GraphqlActor::InstallationToken(token_info.clone()));
    }
    if verify_any_app_jwt(state, &token) {
        return Ok(GraphqlActor::AppJwt);
    }
    Err(BearerTokenError::Invalid)
}

pub fn ensure_repo_permission(
    token: &TokenInfo,
    repo: &str,
    permission: TokenPermission,
    required: PermissionLevel,
) -> Result<(), InstallationTokenAccessError> {
    if !token.allows_repo(repo) {
        return Err(InstallationTokenAccessError::RepoNotAccessible);
    }
    if !token.allows(permission, required) {
        return Err(InstallationTokenAccessError::PermissionDenied);
    }
    Ok(())
}

pub fn ensure_permission(
    token: &TokenInfo,
    permission: TokenPermission,
    required: PermissionLevel,
) -> Result<(), InstallationTokenAccessError> {
    if !token.allows(permission, required) {
        return Err(InstallationTokenAccessError::PermissionDenied);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::derive_public_key_pem;

    fn test_rsa_key() -> String {
        use std::process::Command;
        let output = Command::new("openssl")
            .args([
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                "rsa_keygen_bits:2048",
            ])
            .output()
            .expect("openssl should be available");
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    }

    fn sign_test_jwt(app_id: &str, private_key_pem: &str) -> String {
        use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
        use serde::Serialize;

        #[derive(Serialize)]
        struct Claims {
            iss: String,
            iat: i64,
            exp: i64,
        }

        let now = chrono::Utc::now().timestamp();
        let claims = Claims {
            iss: app_id.to_string(),
            iat: now - 60,
            exp: now + 600,
        };
        let key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes()).unwrap();
        encode(&Header::new(Algorithm::RS256), &claims, &key).unwrap()
    }

    #[test]
    fn verify_valid_jwt() {
        let private_pem = test_rsa_key();
        let public_pem = derive_public_key_pem(&private_pem);
        let jwt = sign_test_jwt("12345", &private_pem);
        let result = verify_app_jwt(&jwt, &public_pem);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "12345");
    }

    #[test]
    fn reject_invalid_jwt() {
        let private_pem = test_rsa_key();
        let public_pem = derive_public_key_pem(&private_pem);
        let result = verify_app_jwt("invalid.jwt.token", &public_pem);
        assert!(result.is_err());
    }
}
