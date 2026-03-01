use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;

/// JWT claims for service-to-service authentication.
#[derive(Debug, Deserialize)]
struct Claims {
    iss: String,
    #[allow(dead_code)]
    iat: u64,
    #[allow(dead_code)]
    exp: u64,
}

/// Authentication mode resolved at startup.
#[derive(Clone)]
pub enum AuthMode {
    /// JWT verification is enabled with the given decoding key.
    Jwt(Arc<DecodingKey>),
    /// Authentication is explicitly disabled (insecure, for development only).
    Disabled,
}

/// Resolve the authentication mode from environment variables.
///
/// Call this once at startup before serving requests. Panics if the
/// configuration is invalid (no public key and insecure mode not enabled).
pub fn resolve_auth_mode() -> AuthMode {
    if let Ok(pem) = std::env::var("ARC_JWT_PUBLIC_KEY") {
        let key = DecodingKey::from_ed_pem(pem.as_bytes())
            .expect("ARC_JWT_PUBLIC_KEY contains an invalid Ed25519 PEM public key");
        AuthMode::Jwt(Arc::new(key))
    } else if std::env::var("ARC_INSECURE_DISABLE_AUTHENTICATION")
        .ok()
        .as_deref()
        == Some("true")
    {
        eprintln!("WARNING: JWT authentication is disabled. Do not use this in production.");
        AuthMode::Disabled
    } else {
        panic!(
            "ARC_JWT_PUBLIC_KEY is not set. Either provide an Ed25519 public key in PEM \
             format or set ARC_INSECURE_DISABLE_AUTHENTICATION=true to allow \
             unauthenticated access (development only)."
        );
    }
}

/// Axum extractor that enforces JWT authentication on a route.
///
/// Add this as a parameter to any handler to require a valid JWT.
/// The `AuthMode` must be added to the router as an Extension.
/// When auth is disabled, the extractor accepts all requests.
pub struct AuthenticatedService;

impl<S: Send + Sync> FromRequestParts<S> for AuthenticatedService {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let auth_mode = parts
            .extensions
            .get::<AuthMode>()
            .expect("AuthMode extension must be added to the router");

        let key = match auth_mode {
            AuthMode::Disabled => return Ok(AuthenticatedService),
            AuthMode::Jwt(key) => key,
        };

        let header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or(StatusCode::UNAUTHORIZED)?;

        let token = header
            .strip_prefix("Bearer ")
            .ok_or(StatusCode::UNAUTHORIZED)?;

        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_required_spec_claims(&["iss", "iat", "exp"]);
        validation.set_issuer(&["arc-web"]);

        let token_data = jsonwebtoken::decode::<Claims>(token, key, &validation)
            .map_err(|_| StatusCode::UNAUTHORIZED)?;

        if token_data.claims.iss != "arc-web" {
            return Err(StatusCode::UNAUTHORIZED);
        }

        Ok(AuthenticatedService)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    async fn protected_handler(_auth: AuthenticatedService) -> impl IntoResponse {
        "ok"
    }

    fn test_router(mode: AuthMode) -> Router {
        Router::new()
            .route("/test", get(protected_handler))
            .layer(axum::Extension(mode))
    }

    fn generate_test_keypair() -> (jsonwebtoken::EncodingKey, DecodingKey) {
        let output = std::process::Command::new("openssl")
            .args(["genpkey", "-algorithm", "Ed25519"])
            .output()
            .expect("openssl must be available for tests");
        let private_pem = output.stdout;

        let output = std::process::Command::new("openssl")
            .args(["pkey", "-pubout"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(&private_pem).unwrap();
                child.wait_with_output()
            })
            .expect("openssl pkey failed");
        let public_pem = output.stdout;

        let encoding =
            jsonwebtoken::EncodingKey::from_ed_pem(&private_pem).expect("invalid private key");
        let decoding = DecodingKey::from_ed_pem(&public_pem).expect("invalid public key");
        (encoding, decoding)
    }

    fn sign_token(key: &jsonwebtoken::EncodingKey, iss: &str, exp_secs: u64) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = serde_json::json!({
            "iss": iss,
            "iat": now,
            "exp": now + exp_secs,
        });
        let header = jsonwebtoken::Header::new(Algorithm::EdDSA);
        jsonwebtoken::encode(&header, &claims, key).expect("failed to sign token")
    }

    #[tokio::test]
    async fn rejects_missing_auth_header() {
        let (_, decoding) = generate_test_keypair();
        let app = test_router(AuthMode::Jwt(Arc::new(decoding)));

        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_invalid_token() {
        let (_, decoding) = generate_test_keypair();
        let app = test_router(AuthMode::Jwt(Arc::new(decoding)));

        let req = Request::builder()
            .uri("/test")
            .header("authorization", "Bearer invalid.token.here")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn accepts_valid_token() {
        let (encoding, decoding) = generate_test_keypair();
        let app = test_router(AuthMode::Jwt(Arc::new(decoding)));

        let token = sign_token(&encoding, "arc-web", 60);

        let req = Request::builder()
            .uri("/test")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rejects_wrong_issuer() {
        let (encoding, decoding) = generate_test_keypair();
        let app = test_router(AuthMode::Jwt(Arc::new(decoding)));

        let token = sign_token(&encoding, "wrong-issuer", 60);

        let req = Request::builder()
            .uri("/test")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_expired_token() {
        let (encoding, decoding) = generate_test_keypair();
        let app = test_router(AuthMode::Jwt(Arc::new(decoding)));

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = serde_json::json!({
            "iss": "arc-web",
            "iat": now - 200,
            "exp": now - 100,
        });
        let header = jsonwebtoken::Header::new(Algorithm::EdDSA);
        let token = jsonwebtoken::encode(&header, &claims, &encoding).unwrap();

        let req = Request::builder()
            .uri("/test")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn disabled_mode_allows_all_requests() {
        let app = test_router(AuthMode::Disabled);

        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
