#![allow(clippy::absolute_paths)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fabro_server::jwt_auth::{AuthMode, AuthStrategy};
use fabro_server::server::{build_router, create_app_state};
use fabro_server::tls::{ClientAuth, build_rustls_config};
use fabro_server::tls_config::TlsSettings;
use tokio::net::TcpListener;

use crate::helpers::api;

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mtls")
        .join(name)
}

fn fixture_pki() -> PkiPaths {
    PkiPaths {
        ca_cert:     fixture_path("ca.crt"),
        server_cert: fixture_path("server.crt"),
        server_key:  fixture_path("server.key"),
        client_cert: fixture_path("client.crt"),
        client_key:  fixture_path("client.key"),
    }
}

struct PkiPaths {
    ca_cert:     PathBuf,
    server_cert: PathBuf,
    server_key:  PathBuf,
    client_cert: PathBuf,
    client_key:  PathBuf,
}

/// Start a TLS server on a random port, returning the bound address.
async fn start_tls_server(
    tls_settings: &TlsSettings,
    client_auth: ClientAuth,
    auth_mode: AuthMode,
) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let rustls_config = build_rustls_config(tls_settings, client_auth);
    let tls_acceptor = tokio_rustls::TlsAcceptor::from(rustls_config);

    let state = create_app_state();
    let router = build_router(state, auth_mode);

    tokio::spawn(async move {
        let _ = fabro_server::tls::serve_tls(listener, tls_acceptor, router).await;
    });

    addr
}

/// Build a reqwest client with the given CA cert and optional client identity.
fn build_client(
    ca_cert_path: &Path,
    client_cert_path: Option<&Path>,
    client_key_path: Option<&Path>,
) -> fabro_http::HttpClient {
    let ca_pem = std::fs::read(ca_cert_path).unwrap();
    let ca_cert = fabro_http::tls::Certificate::from_pem(&ca_pem).unwrap();

    let mut builder = fabro_http::HttpClientBuilder::new()
        .add_root_certificate(ca_cert)
        .no_proxy()
        .use_rustls_tls();

    if let (Some(cert_path), Some(key_path)) = (client_cert_path, client_key_path) {
        let cert_pem = std::fs::read(cert_path).unwrap();
        let key_pem = std::fs::read(key_path).unwrap();
        let mut identity_pem = cert_pem;
        identity_pem.extend_from_slice(&key_pem);
        let identity = fabro_http::tls::Identity::from_pem(&identity_pem).unwrap();
        builder = builder.identity(identity);
    }

    builder.build().unwrap()
}

fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[tokio::test]
async fn mtls_accepts_valid_client_cert() {
    install_crypto_provider();
    let pki = fixture_pki();

    let tls_settings = TlsSettings {
        cert: pki.server_cert.clone(),
        key:  pki.server_key.clone(),
        ca:   pki.ca_cert.clone(),
    };

    let auth_mode = AuthMode::Strategies(vec![AuthStrategy::Mtls]);
    let addr = start_tls_server(&tls_settings, ClientAuth::Required, auth_mode).await;

    let client = build_client(&pki.ca_cert, Some(&pki.client_cert), Some(&pki.client_key));

    let response = client
        .get(format!("https://127.0.0.1:{}{}", addr.port(), api("/runs")))
        .send()
        .await
        .expect("request with valid client cert should succeed");

    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn mtls_only_rejects_wrong_ca_client_cert() {
    install_crypto_provider();
    let pki = fixture_pki();

    let tls_settings = TlsSettings {
        cert: pki.server_cert.clone(),
        key:  pki.server_key.clone(),
        ca:   pki.ca_cert.clone(),
    };

    let auth_mode = AuthMode::Strategies(vec![AuthStrategy::Mtls]);
    let addr = start_tls_server(&tls_settings, ClientAuth::Required, auth_mode).await;

    let wrong_client_cert = fixture_path("wrong-client.crt");
    let wrong_client_key = fixture_path("wrong-client.key");

    // Client trusts the REAL server CA, but presents a cert from the WRONG CA
    let client = build_client(
        &pki.ca_cert,
        Some(&wrong_client_cert),
        Some(&wrong_client_key),
    );

    let result = client
        .get(format!("https://127.0.0.1:{}{}", addr.port(), api("/runs")))
        .send()
        .await;

    // Server should reject the TLS handshake — the wrong CA client cert
    // will cause a connection error (not an HTTP error)
    assert!(
        result.is_err(),
        "request with wrong-CA client cert should fail at TLS level, but got: {:?}",
        result.unwrap().status()
    );
}

#[tokio::test]
async fn mtls_only_rejects_no_client_cert() {
    install_crypto_provider();
    let pki = fixture_pki();

    let tls_settings = TlsSettings {
        cert: pki.server_cert.clone(),
        key:  pki.server_key.clone(),
        ca:   pki.ca_cert.clone(),
    };

    // mTLS is the ONLY strategy -> client cert is required at TLS level
    let auth_mode = AuthMode::Strategies(vec![AuthStrategy::Mtls]);
    let addr = start_tls_server(&tls_settings, ClientAuth::Required, auth_mode).await;

    // Client trusts the server CA but presents NO client cert
    let client = build_client(&pki.ca_cert, None, None);

    let result = client
        .get(format!("https://127.0.0.1:{}{}", addr.port(), api("/runs")))
        .send()
        .await;

    // Server requires client cert -> TLS handshake should fail
    assert!(
        result.is_err(),
        "request without client cert should fail when mTLS is the only strategy, but got: {:?}",
        result.unwrap().status()
    );
}

fn fixture_jwt_keypair() -> (jsonwebtoken::EncodingKey, jsonwebtoken::DecodingKey) {
    let private_pem = std::fs::read(fixture_path("jwt-ed25519-private.pem")).unwrap();
    let public_pem = std::fs::read(fixture_path("jwt-ed25519-public.pem")).unwrap();

    let encoding =
        jsonwebtoken::EncodingKey::from_ed_pem(&private_pem).expect("invalid private key");
    let decoding = jsonwebtoken::DecodingKey::from_ed_pem(&public_pem).expect("invalid public key");
    (encoding, decoding)
}

fn sign_jwt(key: &jsonwebtoken::EncodingKey, sub: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = serde_json::json!({
        "iss": "fabro-web",
        "iat": now,
        "exp": now + 60,
        "sub": sub,
    });
    let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA);
    jsonwebtoken::encode(&header, &claims, key).expect("failed to sign token")
}

#[tokio::test]
async fn mtls_and_jwt_accepts_valid_jwt_without_client_cert() {
    install_crypto_provider();
    let pki = fixture_pki();

    let tls_settings = TlsSettings {
        cert: pki.server_cert.clone(),
        key:  pki.server_key.clone(),
        ca:   pki.ca_cert.clone(),
    };

    let (encoding_key, decoding_key) = fixture_jwt_keypair();

    // Both mTLS and JWT strategies; mTLS is optional since JWT is also present
    let auth_mode = AuthMode::Strategies(vec![AuthStrategy::Mtls, AuthStrategy::Jwt {
        key:               Arc::new(decoding_key),
        validation:        Arc::new(fabro_server::jwt_auth::jwt_validation()),
        allowed_usernames: vec!["brynary".to_string()],
    }]);
    let addr = start_tls_server(&tls_settings, ClientAuth::Optional, auth_mode).await;

    // Client trusts the server CA but presents NO client cert
    let client = build_client(&pki.ca_cert, None, None);

    let token = sign_jwt(&encoding_key, "https://github.com/brynary");

    let response = client
        .get(format!("https://127.0.0.1:{}{}", addr.port(), api("/runs")))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request with valid JWT and no client cert should succeed");

    assert_eq!(
        response.status(),
        200,
        "valid JWT should be accepted when strategies = [mtls, jwt]"
    );
}
