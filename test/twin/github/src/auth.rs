use axum::http::HeaderMap;

use crate::state::{AppState, PermissionLevel, TokenInfo, TokenPermission};

/// Verify a GitHub App JWT (RS256) and return the `iss` claim (app_id).
///
/// Accepts a **public** key PEM. The caller obtains this from
/// `RegisteredApp::public_key_pem`, which is derived from the private key
/// during `AppState::register_app`.
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
    use crate::test_support::{sign_test_jwt, test_rsa_private_key, test_rsa_public_key};

    #[test]
    fn verify_valid_jwt() {
        let private_pem = test_rsa_private_key();
        let jwt = sign_test_jwt("12345", private_pem);
        let result = verify_app_jwt(&jwt, test_rsa_public_key());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "12345");
    }

    #[test]
    fn reject_invalid_jwt() {
        let derived_public_pem = derive_public_key_pem(test_rsa_private_key());
        assert_eq!(derived_public_pem, test_rsa_public_key());
        let result = verify_app_jwt("invalid.jwt.token", test_rsa_public_key());
        assert!(result.is_err());
    }
}
