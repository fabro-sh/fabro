use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use rustls_pki_types::CertificateDer;
use serde::Deserialize;
use tracing::warn;

use crate::error::ApiError;
use crate::web_auth::SessionCookie;
use fabro_config::server::ApiSettings;
use fabro_types::RunAuthMethod;

/// JWT claims for service-to-service authentication.
#[derive(Debug, Deserialize)]
struct Claims {
    #[allow(dead_code)]
    iss: String,
    #[allow(dead_code)]
    iat: u64,
    #[allow(dead_code)]
    exp: u64,
    sub: Option<String>,
}

/// A single authentication strategy resolved at startup.
#[derive(Clone)]
pub enum AuthStrategy {
    Jwt {
        key: Arc<DecodingKey>,
        validation: Arc<Validation>,
        allowed_usernames: Vec<String>,
    },
    Cookie,
    Mtls,
}

pub fn jwt_validation() -> Validation {
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.set_required_spec_claims(&["iss", "iat", "exp"]);
    validation.set_issuer(&["fabro-web"]);
    validation
}

/// Authentication mode resolved at startup.
#[derive(Clone)]
pub enum AuthMode {
    /// One or more strategies to try in order.
    Strategies(Vec<AuthStrategy>),
    /// Authentication is explicitly disabled (used for demo requests via `X-Fabro-Demo: 1` header).
    Disabled,
}

/// Peer certificates extracted from the TLS connection, inserted as a request extension.
#[derive(Clone)]
pub struct PeerCertificates(pub Option<Vec<CertificateDer<'static>>>);

/// Decode a PEM env var that may be raw PEM or base64-encoded PEM.
pub fn decode_pem_env(name: &str, value: &str) -> String {
    if value.starts_with("-----") {
        return value.to_string();
    }
    let bytes = base64::Engine::decode(&BASE64_STANDARD, value)
        .unwrap_or_else(|e| panic!("{name} is not valid PEM or base64: {e}"));
    String::from_utf8(bytes)
        .unwrap_or_else(|e| panic!("{name} base64 decoded to invalid UTF-8: {e}"))
}

/// Resolve the authentication mode from the API config section.
///
/// Call this once at startup before serving requests. Panics if the
/// configuration is invalid (JWT strategy but no public key, or mTLS without TLS config).
pub fn resolve_auth_mode(api_settings: &ApiSettings, allowed_usernames: &[String]) -> AuthMode {
    resolve_auth_mode_with_lookup(api_settings, allowed_usernames, |name| {
        std::env::var(name).ok()
    })
}

pub fn resolve_auth_mode_with_lookup<F>(
    api_settings: &ApiSettings,
    allowed_usernames: &[String],
    lookup: F,
) -> AuthMode
where
    F: Fn(&str) -> Option<String>,
{
    use fabro_config::server::ApiAuthStrategy;

    if api_settings.authentication_strategies.is_empty()
        && std::env::var("FABRO_LOCAL_NO_AUTH").ok().as_deref() == Some("1")
    {
        warn!(
            "No authentication strategies configured; allowing unauthenticated local daemon access"
        );
        return AuthMode::Disabled;
    }

    if api_settings.authentication_strategies.is_empty() {
        warn!("No authentication strategies configured; all requests will be rejected");
    }

    let mut strategies = Vec::new();
    if lookup("SESSION_SECRET").is_some() {
        strategies.push(AuthStrategy::Cookie);
    }

    strategies.extend(api_settings
        .authentication_strategies
        .iter()
        .map(|s| match s {
            ApiAuthStrategy::Jwt => {
                let raw = lookup("FABRO_JWT_PUBLIC_KEY").unwrap_or_else(|| {
                    panic!(
                        "FABRO_JWT_PUBLIC_KEY is not set. Provide an Ed25519 public key in PEM \
                         format (or base64-encoded PEM) for JWT authentication."
                    )
                });
                let pem = decode_pem_env("FABRO_JWT_PUBLIC_KEY", &raw);
                let key = DecodingKey::from_ed_pem(pem.as_bytes())
                    .expect("FABRO_JWT_PUBLIC_KEY contains an invalid Ed25519 PEM public key");
                AuthStrategy::Jwt {
                    key: Arc::new(key),
                    validation: Arc::new(jwt_validation()),
                    allowed_usernames: allowed_usernames.to_vec(),
                }
            }
            ApiAuthStrategy::Mtls => {
                assert!(
                    api_settings.tls.is_some(),
                    "mTLS authentication strategy requires [api.tls] configuration with cert, key, and ca"
                );
                AuthStrategy::Mtls
            }
        }));

    AuthMode::Strategies(strategies)
}

/// Extract the login from JWT claims.
fn extract_jwt_login(parts: &Parts, key: &DecodingKey, validation: &Validation) -> Option<String> {
    let header = parts
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())?;
    let token = header.strip_prefix("Bearer ")?;
    let token_data = jsonwebtoken::decode::<Claims>(token, key, validation).ok()?;
    token_data
        .claims
        .sub
        .as_deref()
        .and_then(|s| s.rsplit('/').next())
        .map(String::from)
}

/// Extract the CN from mTLS peer certificates.
fn extract_mtls_cn(parts: &Parts) -> Option<String> {
    let peer_certs = parts
        .extensions
        .get::<PeerCertificates>()
        .and_then(|pc| pc.0.as_ref())?;
    let cert = peer_certs.first()?;
    let (_, parsed) = x509_parser::parse_x509_certificate(cert).ok()?;
    let cn = parsed
        .subject()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .map(String::from);
    cn
}

/// Try to authenticate via JWT.
fn try_jwt(
    parts: &Parts,
    key: &DecodingKey,
    validation: &Validation,
    allowed_usernames: &[String],
) -> Result<(), ApiError> {
    let header = parts
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(ApiError::unauthorized)?;

    let token = header
        .strip_prefix("Bearer ")
        .ok_or_else(ApiError::unauthorized)?;

    let token_data = jsonwebtoken::decode::<Claims>(token, key, validation)
        .map_err(|_| ApiError::unauthorized())?;

    // Fail closed: if no usernames are allowed, reject all requests
    if allowed_usernames.is_empty() {
        return Err(ApiError::forbidden());
    }

    // Extract GitHub username from sub claim URL (last path segment)
    let username = token_data
        .claims
        .sub
        .as_deref()
        .and_then(|s| s.rsplit('/').next())
        .ok_or_else(ApiError::forbidden)?;

    if !allowed_usernames.iter().any(|u| u == username) {
        return Err(ApiError::forbidden());
    }

    Ok(())
}

/// Try to authenticate via mTLS peer certificates.
fn try_mtls(parts: &Parts) -> Result<(), ApiError> {
    let peer_certs = parts
        .extensions
        .get::<PeerCertificates>()
        .and_then(|pc| pc.0.as_ref())
        .ok_or_else(ApiError::unauthorized)?;

    if peer_certs.is_empty() {
        return Err(ApiError::unauthorized());
    }

    // Verify we can parse the leaf certificate and extract a CN
    let cert = &peer_certs[0];
    let (_, parsed) =
        x509_parser::parse_x509_certificate(cert).map_err(|_| ApiError::unauthorized())?;

    parsed
        .subject()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .ok_or_else(ApiError::unauthorized)?;

    Ok(())
}

fn try_cookie(parts: &Parts) -> Result<(), ApiError> {
    parts
        .extensions
        .get::<SessionCookie>()
        .map(|_| ())
        .ok_or_else(ApiError::unauthorized)
}

/// Axum extractor that enforces authentication on a route.
///
/// Tries each configured strategy in order. The first successful match wins.
/// The `AuthMode` must be added to the router as an Extension.
/// When auth is disabled, the extractor accepts all requests.
pub struct AuthenticatedService;

pub fn authenticate_service_parts(parts: &Parts) -> Result<(), ApiError> {
    let auth_mode = parts
        .extensions
        .get::<AuthMode>()
        .expect("AuthMode extension must be added to the router");

    let strategies = match auth_mode {
        AuthMode::Disabled => return Ok(()),
        AuthMode::Strategies(strategies) => strategies,
    };

    if strategies.is_empty() {
        return Err(ApiError::unauthorized());
    }

    let mut last_err = ApiError::unauthorized();

    for strategy in strategies {
        let result = match strategy {
            AuthStrategy::Mtls => try_mtls(parts),
            AuthStrategy::Cookie => try_cookie(parts),
            AuthStrategy::Jwt {
                key,
                validation,
                allowed_usernames,
            } => try_jwt(parts, key, validation, allowed_usernames),
        };
        match result {
            Ok(()) => return Ok(()),
            Err(err) => last_err = err,
        }
    }

    Err(last_err)
}

impl<S: Send + Sync> FromRequestParts<S> for AuthenticatedService {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        authenticate_service_parts(parts)?;
        Ok(Self)
    }
}

/// Axum extractor that authenticates and extracts the request subject.
pub struct AuthenticatedSubject {
    pub login: Option<String>,
    pub auth_method: RunAuthMethod,
}

impl<S: Send + Sync> FromRequestParts<S> for AuthenticatedSubject {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let auth_mode = parts
            .extensions
            .get::<AuthMode>()
            .expect("AuthMode extension must be added to the router");

        let strategies = match auth_mode {
            AuthMode::Disabled => {
                return Ok(Self {
                    login: None,
                    auth_method: RunAuthMethod::Disabled,
                });
            }
            AuthMode::Strategies(strategies) => strategies,
        };

        if strategies.is_empty() {
            return Err(ApiError::unauthorized());
        }

        let mut last_err = ApiError::unauthorized();

        for strategy in strategies {
            match strategy {
                AuthStrategy::Cookie => {
                    if let Some(session) = parts.extensions.get::<SessionCookie>() {
                        return Ok(Self {
                            login: Some(session.login.clone()),
                            auth_method: RunAuthMethod::Cookie,
                        });
                    }
                    last_err = ApiError::unauthorized();
                }
                AuthStrategy::Jwt {
                    key,
                    validation,
                    allowed_usernames,
                } => {
                    if try_jwt(parts, key, validation, allowed_usernames).is_ok() {
                        if let Some(login) = extract_jwt_login(parts, key, validation) {
                            return Ok(Self {
                                login: Some(login),
                                auth_method: RunAuthMethod::Jwt,
                            });
                        }
                    }
                    last_err = ApiError::unauthorized();
                }
                AuthStrategy::Mtls => {
                    if try_mtls(parts).is_ok() {
                        if let Some(login) = extract_mtls_cn(parts) {
                            return Ok(Self {
                                login: Some(login),
                                auth_method: RunAuthMethod::Mtls,
                            });
                        }
                    }
                    last_err = ApiError::unauthorized();
                }
            }
        }

        Err(last_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use tower::ServiceExt;

    use crate::web_auth::SessionCookie;

    async fn protected_handler(_auth: AuthenticatedService) -> impl IntoResponse {
        "ok"
    }

    async fn subject_handler(subject: AuthenticatedSubject) -> impl IntoResponse {
        Json(serde_json::json!({
            "login": subject.login,
            "auth_method": subject.auth_method,
        }))
    }

    fn test_router(mode: AuthMode) -> Router {
        Router::new()
            .route("/test", get(protected_handler))
            .layer(axum::Extension(mode))
    }

    fn subject_router(mode: AuthMode) -> Router {
        Router::new()
            .route("/subject", get(subject_handler))
            .layer(axum::Extension(mode))
    }

    async fn response_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
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

    fn sign_token(
        key: &jsonwebtoken::EncodingKey,
        iss: &str,
        exp_secs: u64,
        sub: Option<&str>,
    ) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut claims = serde_json::json!({
            "iss": iss,
            "iat": now,
            "exp": now + exp_secs,
        });
        if let Some(sub) = sub {
            claims["sub"] = serde_json::Value::String(sub.to_string());
        }
        let header = jsonwebtoken::Header::new(Algorithm::EdDSA);
        jsonwebtoken::encode(&header, &claims, key).expect("failed to sign token")
    }

    fn jwt_mode(decoding: DecodingKey, allowed_usernames: Vec<&str>) -> AuthMode {
        AuthMode::Strategies(vec![AuthStrategy::Jwt {
            key: Arc::new(decoding),
            validation: Arc::new(jwt_validation()),
            allowed_usernames: allowed_usernames.into_iter().map(String::from).collect(),
        }])
    }

    /// Build a test request with PeerCertificates extension pre-inserted.
    fn request_with_peer_certs(
        uri: &str,
        certs: Option<Vec<CertificateDer<'static>>>,
    ) -> Request<Body> {
        let mut req = Request::builder().uri(uri).body(Body::empty()).unwrap();
        req.extensions_mut().insert(PeerCertificates(certs));
        req
    }

    /// Generate a self-signed CA + client cert for mTLS testing.
    /// Returns (ca_cert_der, client_cert_der) where client_cert_der has the given CN.
    fn generate_test_client_cert(cn: &str) -> CertificateDer<'static> {
        use std::process::{Command, Stdio};

        // Generate CA key + self-signed cert
        let ca_key = Command::new("openssl")
            .args(["genpkey", "-algorithm", "Ed25519"])
            .output()
            .expect("openssl genpkey failed")
            .stdout;

        let ca_cert = {
            let mut child = Command::new("openssl")
                .args([
                    "req",
                    "-new",
                    "-x509",
                    "-key",
                    "/dev/stdin",
                    "-days",
                    "1",
                    "-subj",
                    "/CN=TestCA",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("openssl req failed");
            std::io::Write::write_all(&mut child.stdin.take().unwrap(), &ca_key).unwrap();
            child.wait_with_output().unwrap().stdout
        };

        // Generate client key
        let client_key = Command::new("openssl")
            .args(["genpkey", "-algorithm", "Ed25519"])
            .output()
            .expect("openssl genpkey failed")
            .stdout;

        // Generate client CSR
        let subj = format!("/CN={cn}");
        let client_csr = {
            let mut child = Command::new("openssl")
                .args(["req", "-new", "-key", "/dev/stdin", "-subj", &subj])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("openssl req failed");
            std::io::Write::write_all(&mut child.stdin.take().unwrap(), &client_key).unwrap();
            child.wait_with_output().unwrap().stdout
        };

        // Sign client cert with CA
        let dir = tempfile::tempdir().unwrap();
        let ca_cert_path = dir.path().join("ca.crt");
        let ca_key_path = dir.path().join("ca.key");
        let csr_path = dir.path().join("client.csr");
        std::fs::write(&ca_cert_path, &ca_cert).unwrap();
        std::fs::write(&ca_key_path, &ca_key).unwrap();
        std::fs::write(&csr_path, &client_csr).unwrap();

        let client_cert_pem = Command::new("openssl")
            .args([
                "x509",
                "-req",
                "-in",
                csr_path.to_str().unwrap(),
                "-CA",
                ca_cert_path.to_str().unwrap(),
                "-CAkey",
                ca_key_path.to_str().unwrap(),
                "-CAcreateserial",
                "-days",
                "1",
            ])
            .output()
            .expect("openssl x509 failed")
            .stdout;

        // Convert PEM to DER
        let client_cert_der = {
            let mut child = Command::new("openssl")
                .args(["x509", "-outform", "DER"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("openssl x509 DER conversion failed");
            std::io::Write::write_all(&mut child.stdin.take().unwrap(), &client_cert_pem).unwrap();
            child.wait_with_output().unwrap().stdout
        };

        CertificateDer::from(client_cert_der)
    }

    // --- JWT tests (updated for Strategies wrapper) ---

    #[tokio::test]
    async fn rejects_missing_auth_header() {
        let (_, decoding) = generate_test_keypair();
        let app = test_router(jwt_mode(decoding, vec!["brynary"]));

        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_invalid_token() {
        let (_, decoding) = generate_test_keypair();
        let app = test_router(jwt_mode(decoding, vec!["brynary"]));

        let req = Request::builder()
            .uri("/test")
            .header("authorization", "Bearer invalid.token.here")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn accepts_valid_token_with_matching_username() {
        let (encoding, decoding) = generate_test_keypair();
        let app = test_router(jwt_mode(decoding, vec!["brynary"]));

        let token = sign_token(
            &encoding,
            "fabro-web",
            60,
            Some("https://github.com/brynary"),
        );

        let req = Request::builder()
            .uri("/test")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rejects_token_with_non_matching_username() {
        let (encoding, decoding) = generate_test_keypair();
        let app = test_router(jwt_mode(decoding, vec!["brynary"]));

        let token = sign_token(
            &encoding,
            "fabro-web",
            60,
            Some("https://github.com/someone-else"),
        );

        let req = Request::builder()
            .uri("/test")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rejects_token_with_missing_sub() {
        let (encoding, decoding) = generate_test_keypair();
        let app = test_router(jwt_mode(decoding, vec!["brynary"]));

        let token = sign_token(&encoding, "fabro-web", 60, None);

        let req = Request::builder()
            .uri("/test")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rejects_when_allowed_usernames_empty() {
        let (encoding, decoding) = generate_test_keypair();
        let app = test_router(jwt_mode(decoding, vec![]));

        let token = sign_token(
            &encoding,
            "fabro-web",
            60,
            Some("https://github.com/brynary"),
        );

        let req = Request::builder()
            .uri("/test")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rejects_wrong_issuer() {
        let (encoding, decoding) = generate_test_keypair();
        let app = test_router(jwt_mode(decoding, vec!["brynary"]));

        let token = sign_token(
            &encoding,
            "wrong-issuer",
            60,
            Some("https://github.com/brynary"),
        );

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
        let app = test_router(jwt_mode(decoding, vec!["brynary"]));

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = serde_json::json!({
            "iss": "fabro-web",
            "iat": now - 200,
            "exp": now - 100,
            "sub": "https://github.com/brynary",
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

    #[tokio::test]
    async fn disabled_mode_extracts_disabled_subject() {
        let app = subject_router(AuthMode::Disabled);

        let req = Request::builder()
            .uri("/subject")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["login"], serde_json::Value::Null);
        assert_eq!(body["auth_method"], "disabled");
    }

    #[tokio::test]
    async fn jwt_subject_extracts_login_and_auth_method() {
        let (encoding, decoding) = generate_test_keypair();
        let app = subject_router(jwt_mode(decoding, vec!["brynary"]));

        let token = sign_token(
            &encoding,
            "fabro-web",
            60,
            Some("https://github.com/brynary"),
        );

        let req = Request::builder()
            .uri("/subject")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["login"], "brynary");
        assert_eq!(body["auth_method"], "jwt");
    }

    #[tokio::test]
    async fn cookie_subject_extracts_login_and_auth_method() {
        let app = subject_router(AuthMode::Strategies(vec![AuthStrategy::Cookie]));

        let mut req = Request::builder()
            .uri("/subject")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(SessionCookie {
            login: "brynary".to_string(),
            name: "Brynary".to_string(),
            email: "b@example.com".to_string(),
            avatar_url: "https://example.com/avatar.png".to_string(),
            user_url: "https://github.com/brynary".to_string(),
            github_id: 1,
            exp: 9999999999,
        });

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["login"], "brynary");
        assert_eq!(body["auth_method"], "cookie");
    }

    #[tokio::test]
    async fn empty_strategies_rejects() {
        let app = test_router(AuthMode::Strategies(vec![]));

        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // --- mTLS tests ---

    #[tokio::test]
    async fn mtls_accepts_valid_peer_cert() {
        let app = test_router(AuthMode::Strategies(vec![AuthStrategy::Mtls]));

        let cert = generate_test_client_cert("testuser");
        let req = request_with_peer_certs("/test", Some(vec![cert]));

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn mtls_rejects_no_peer_certs() {
        let app = test_router(AuthMode::Strategies(vec![AuthStrategy::Mtls]));

        let req = request_with_peer_certs("/test", None);

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn mtls_rejects_empty_peer_certs() {
        let app = test_router(AuthMode::Strategies(vec![AuthStrategy::Mtls]));

        let req = request_with_peer_certs("/test", Some(vec![]));

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn mtls_rejects_when_no_peer_certs_extension() {
        let app = test_router(AuthMode::Strategies(vec![AuthStrategy::Mtls]));

        // No PeerCertificates extension at all (plain HTTP path)
        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn mtls_subject_extracts_login_and_auth_method() {
        let app = subject_router(AuthMode::Strategies(vec![AuthStrategy::Mtls]));

        let cert = generate_test_client_cert("brynary");
        let req = request_with_peer_certs("/subject", Some(vec![cert]));

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["login"], "brynary");
        assert_eq!(body["auth_method"], "mtls");
    }

    // --- Multi-strategy tests ---

    #[tokio::test]
    async fn jwt_and_mtls_accepts_valid_cert_no_jwt() {
        let (_, decoding) = generate_test_keypair();
        let mode = AuthMode::Strategies(vec![
            AuthStrategy::Jwt {
                key: Arc::new(decoding),
                validation: Arc::new(jwt_validation()),
                allowed_usernames: vec!["brynary".to_string()],
            },
            AuthStrategy::Mtls,
        ]);
        let app = test_router(mode);

        let cert = generate_test_client_cert("brynary");
        let req = request_with_peer_certs("/test", Some(vec![cert]));

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn mtls_and_jwt_falls_back_to_jwt() {
        let (encoding, decoding) = generate_test_keypair();
        let mode = AuthMode::Strategies(vec![
            AuthStrategy::Mtls,
            AuthStrategy::Jwt {
                key: Arc::new(decoding),
                validation: Arc::new(jwt_validation()),
                allowed_usernames: vec!["brynary".to_string()],
            },
        ]);
        let app = test_router(mode);

        let token = sign_token(
            &encoding,
            "fabro-web",
            60,
            Some("https://github.com/brynary"),
        );

        // No peer certs, but valid JWT
        let mut req = Request::builder()
            .uri("/test")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(PeerCertificates(None));

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
