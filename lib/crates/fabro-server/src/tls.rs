use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::ConnectInfo;
use fabro_types::settings::{InterpString, TlsConfig};
use rustls::ServerConfig;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tracing::error;

/// Build a rustls `ServerConfig` from the `[server.listen.tls]` configuration.
pub fn build_rustls_config(tls_settings: &TlsConfig) -> anyhow::Result<Arc<ServerConfig>> {
    let cert = resolve_path(&tls_settings.cert)?;
    let key_path = resolve_path(&tls_settings.key)?;

    let certs = load_certs(&cert);
    let key = load_private_key(&key_path);

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("invalid server certificate or key");

    Ok(Arc::new(config))
}

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

            let io = TokioIo::new(tls_stream);

            let service = service_fn(move |mut req: hyper::Request<Incoming>| {
                let mut router = router.clone();
                async move {
                    req.extensions_mut().insert(ConnectInfo(remote_addr));
                    router.call(req).await
                }
            });

            if let Err(e) = builder.serve_connection(io, service).await {
                error!(%remote_addr, "connection error: {e}");
            }
        });
    }
}

pub use fabro_config::expand_tilde;

fn resolve_path(value: &InterpString) -> anyhow::Result<std::path::PathBuf> {
    let resolved = value
        .resolve(|name| std::env::var(name).ok())
        .with_context(|| format!("failed to resolve {}", value.as_source()))?;
    Ok(expand_tilde(Path::new(&resolved.value)))
}

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
