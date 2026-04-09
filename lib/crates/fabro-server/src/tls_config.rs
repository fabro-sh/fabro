//! Resolved TLS material extracted from `[server.listen.tls]`.
//!
//! This module owns the `(cert, key, ca)` triple that the rustls config
//! builder in [`crate::tls`] consumes when the server is listening on TCP
//! with mTLS enabled. It lives outside `jwt_auth.rs` because TLS material
//! is a listen-side concern, not an authentication strategy — the auth
//! resolver only cares about *whether* TLS is present (for mTLS support),
//! not about its contents.

use std::path::PathBuf;

use fabro_types::settings::SettingsFile;
use fabro_types::settings::interp::InterpString;
use fabro_types::settings::server::ServerListenLayer;

/// Resolved TLS material used by the rustls config builder in
/// [`crate::tls`] when the server is listening on TCP with
/// `[server.listen.tls]` set.
#[derive(Debug, Clone, PartialEq)]
pub struct TlsSettings {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub ca: PathBuf,
}

impl TlsSettings {
    /// Extract the `[server.listen.tls]` subtree out of a `SettingsFile`.
    /// Returns `None` when the server is on Unix sockets, TLS is unset, or
    /// any of the three fields is missing.
    #[must_use]
    pub fn from_settings(file: &SettingsFile) -> Option<Self> {
        let listen = file.server.as_ref()?.listen.as_ref()?;
        let tls = match listen {
            ServerListenLayer::Tcp { tls, .. } => tls.as_ref()?,
            ServerListenLayer::Unix { .. } => return None,
        };
        let cert = tls.cert.as_ref().map(InterpString::as_source)?;
        let key = tls.key.as_ref().map(InterpString::as_source)?;
        let ca = tls.ca.as_ref().map(InterpString::as_source)?;
        Some(Self {
            cert: cert.into(),
            key: key.into(),
            ca: ca.into(),
        })
    }
}
