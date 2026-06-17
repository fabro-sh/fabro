//! The server-owned process-environment lookup facade.
//!
//! Server control-plane settings (storage root, listen path, web/api URL,
//! object-store coordinates, GitHub App identifiers) are plain `String` and do
//! not interpolate; deployment-time late binding goes through native env reads
//! (e.g. `FABRO_WEB_URL`, read in `canonical_origin`). This module owns the
//! single process-env lookup facade those reads — and server secret reads — go
//! through; do not add per-module copies.

/// The server-owned process-env lookup facade for native env reads and server
/// configuration/secret reads.
#[expect(
    clippy::disallowed_methods,
    reason = "server configuration and secret reads own this process-env lookup facade"
)]
pub(crate) fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}
