//! Transitional server-domain runtime types.
//!
//! Only the three types that the auth resolver (`jwt_auth.rs`) and the
//! TLS loader (`tls.rs`) still consume remain here. Stage 6.6g will
//! rewrite those consumers to walk the v2 `server.auth.api` / `server.listen.tls`
//! subtrees directly, at which point this file goes away and the
//! legacy runtime type module tree is fully gone.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Authentication strategy flag consumed by `resolve_auth_mode_with_lookup`.
#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiAuthStrategy {
    Jwt,
    Mtls,
}

/// mTLS material loaded by `fabro_server::tls::*`.
#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct TlsSettings {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub ca: PathBuf,
}

/// Shim `ApiSettings` that `fabro_server::serve::build_legacy_api_settings`
/// projects out of the v2 tree so the pre-v2 `resolve_auth_mode_with_lookup`
/// signature keeps working until Stage 6.6g rewrites it.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize)]
pub struct ApiSettings {
    #[serde(default)]
    pub authentication_strategies: Vec<ApiAuthStrategy>,
    pub tls: Option<TlsSettings>,
}
