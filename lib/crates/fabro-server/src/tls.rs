use std::path::Path;
use std::sync::Arc;
use std::{future::Future, pin::Pin};

use rustls::ServerConfig;
use rustls::server::WebPkiClientVerifier;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tracing::error;

use fabro_config::server::TlsSettings;

use crate::jwt_auth::PeerCertificates;

/// How client certificates should be verified.
#[derive(Clone, Copy)]
pub enum ClientAuth {
    /// No client certificates requested (TLS encryption only).
    None,
    /// Client certificates required; reject connections without one.
    Required,
    /// Client certificates requested but not required (multi-strategy fallback).
    Optional,
}

/// Build a rustls `ServerConfig` from the `[api.tls]` configuration.
pub fn build_rustls_config(
    tls_settings: &TlsSettings,
    client_auth: ClientAuth,
) -> Arc<ServerConfig> {
    let certs = load_certs(&tls_settings.cert);
    let key = load_private_key(&tls_settings.key);

    let config = match client_auth {
        ClientAuth::None => ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .expect("invalid server certificate or key"),
        ClientAuth::Required | ClientAuth::Optional => {
            let ca_certs = load_certs(&tls_settings.ca);
            let mut root_store = rustls::RootCertStore::empty();
            for cert in ca_certs {
                root_store
                    .add(cert)
                    .expect("failed to add CA certificate to root store");
            }

            let builder = WebPkiClientVerifier::builder(Arc::new(root_store));
            let verifier = if matches!(client_auth, ClientAuth::Optional) {
                builder.allow_unauthenticated()
            } else {
                builder
            }
            .build()
            .expect("failed to build client verifier");

            ServerConfig::builder()
                .with_client_cert_verifier(verifier)
                .with_single_cert(certs, key)
                .expect("invalid server certificate or key")
        }
    };

    Arc::new(config)
}

/// Serve requests over TLS, extracting peer certificates into request extensions.
pub async fn serve_tls(
    listener: TcpListener,
    tls_acceptor: tokio_rustls::TlsAcceptor,
    router: axum::Router,
) -> anyhow::Result<()> {
    serve_tls_with_shutdown(listener, tls_acceptor, router, std::future::pending()).await
}

/// Serve requests over TLS until the supplied shutdown future resolves.
pub async fn serve_tls_with_shutdown<F>(
    listener: TcpListener,
    tls_acceptor: tokio_rustls::TlsAcceptor,
    router: axum::Router,
    shutdown: F,
) -> anyhow::Result<()>
where
    F: Future<Output = ()> + Send,
{
    use hyper::body::Incoming;
    use hyper::service::service_fn;
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto::Builder;
    use tower_service::Service;

    let builder = Builder::new(TokioExecutor::new());
    let mut shutdown = Pin::from(Box::new(shutdown));

    loop {
        let accepted = tokio::select! {
            () = &mut shutdown => return Ok(()),
            accepted = listener.accept() => accepted?,
        };
        let (tcp_stream, remote_addr) = accepted;

        let tls_acceptor = tls_acceptor.clone();
        let router = router.clone();
        let builder = builder.clone();

        tokio::spawn(async move {
            let tls_stream = match tls_acceptor.accept(tcp_stream).await {
                Ok(s) => s,
                Err(e) => {
                    error!(%remote_addr, "TLS handshake failed: {e}");
                    return;
                }
            };

            // Extract peer certificates once per connection (not per request)
            let (_, server_conn) = tls_stream.get_ref();
            let peer_certs = PeerCertificates(
                server_conn
                    .peer_certificates()
                    .map(<[rustls_pki_types::CertificateDer<'_>]>::to_vec),
            );

            let io = TokioIo::new(tls_stream);

            let service = service_fn(move |mut req: hyper::Request<Incoming>| {
                req.extensions_mut().insert(peer_certs.clone());
                let mut router = router.clone();
                async move { router.call(req).await }
            });

            if let Err(e) = builder.serve_connection(io, service).await {
                error!(%remote_addr, "connection error: {e}");
            }
        });
    }
}

pub use fabro_config::expand_tilde;

fn load_certs(path: &Path) -> Vec<CertificateDer<'static>> {
    let path = expand_tilde(path);
    let file = std::fs::File::open(&path)
        .unwrap_or_else(|e| panic!("failed to open certificate file {}: {e}", path.display()));
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|e| panic!("failed to parse certificates from {}: {e}", path.display()))
}

fn load_private_key(path: &Path) -> PrivateKeyDer<'static> {
    let path = expand_tilde(path);
    let file = std::fs::File::open(&path)
        .unwrap_or_else(|e| panic!("failed to open private key file {}: {e}", path.display()));
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .unwrap_or_else(|e| panic!("failed to parse private key from {}: {e}", path.display()))
        .unwrap_or_else(|| panic!("no private key found in {}", path.display()))
}
