use std::path::{Path, PathBuf};

use fabro_server::jwt_auth::{AuthMode, ConfiguredAuth};
use fabro_server::server::{build_router, create_app_state};
use fabro_server::tls::build_rustls_config;
use fabro_types::settings::{InterpString, ServerAuthMethod, TlsConfig};
use tokio::net::TcpListener;

use crate::helpers::api;

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mtls")
        .join(name)
}

struct PkiPaths {
    ca_cert: PathBuf,
    server_cert: PathBuf,
    server_key: PathBuf,
}

fn fixture_pki() -> PkiPaths {
    PkiPaths {
        ca_cert: fixture_path("ca.crt"),
        server_cert: fixture_path("server.crt"),
        server_key: fixture_path("server.key"),
    }
}

async fn start_tls_server(tls_settings: &TlsConfig, auth_mode: AuthMode) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let rustls_config = build_rustls_config(tls_settings).unwrap();
    let tls_acceptor = tokio_rustls::TlsAcceptor::from(rustls_config);

    let state = create_app_state();
    let router = build_router(state, auth_mode);

    tokio::spawn(async move {
        let _ = fabro_server::tls::serve_tls(listener, tls_acceptor, router).await;
    });

    addr
}

fn build_client(ca_cert_path: &Path) -> fabro_http::HttpClient {
    let ca_pem = std::fs::read(ca_cert_path).unwrap();
    let ca_cert = fabro_http::tls::Certificate::from_pem(&ca_pem).unwrap();

    fabro_http::HttpClientBuilder::new()
        .add_root_certificate(ca_cert)
        .no_proxy()
        .use_rustls_tls()
        .build()
        .unwrap()
}

fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn tls_settings(pki: &PkiPaths) -> TlsConfig {
    TlsConfig {
        cert: InterpString::parse(&pki.server_cert.to_string_lossy()),
        key: InterpString::parse(&pki.server_key.to_string_lossy()),
    }
}

#[tokio::test]
async fn tls_accepts_requests_without_client_cert() {
    install_crypto_provider();
    let pki = fixture_pki();
    let addr = start_tls_server(&tls_settings(&pki), AuthMode::Disabled).await;
    let client = build_client(&pki.ca_cert);

    let response = client
        .get(format!("https://127.0.0.1:{}{}", addr.port(), api("/runs")))
        .send()
        .await
        .expect("request over TLS should succeed without a client certificate");

    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn tls_dev_token_auth_does_not_require_client_cert() {
    install_crypto_provider();
    let pki = fixture_pki();
    let dev_token = "fabro_dev_abababababababababababababababababababababababababababababababab";
    let auth_mode = AuthMode::Enabled(ConfiguredAuth {
        methods: vec![ServerAuthMethod::DevToken],
        dev_token: Some(dev_token.to_string()),
    });
    let addr = start_tls_server(&tls_settings(&pki), auth_mode).await;
    let client = build_client(&pki.ca_cert);
    let url = format!("https://127.0.0.1:{}{}", addr.port(), api("/runs"));

    let unauthorized = client.get(&url).send().await.unwrap();
    assert_eq!(unauthorized.status(), 401);

    let authorized = client.get(url).bearer_auth(dev_token).send().await.unwrap();
    assert_eq!(authorized.status(), 200);
}
