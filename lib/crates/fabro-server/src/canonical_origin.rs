#![allow(
    clippy::disallowed_types,
    reason = "Canonical origin validation handles the public server origin; it is not credential-bearing log output."
)]

use fabro_static::EnvVars;
use fabro_types::settings::{ServerNamespace, validate_public_url};

use crate::server::EnvLookup;

pub(crate) fn resolve_canonical_origin(
    resolved: &ServerNamespace,
    env_lookup: &EnvLookup,
) -> Result<String, String> {
    let value = effective_web_url(resolved, env_lookup);
    validate_public_url(&value).map_err(|_| canonical_origin_error(&value))
}

/// The effective `server.web.url`.
///
/// `web.url` is plain control-plane config that never interpolates `{{ env.*
/// }}` tokens. Deployment-time late binding instead goes through the native
/// `FABRO_WEB_URL` process-env read: the env override wins when set (and
/// non-empty), otherwise the literal settings value is used.
pub(crate) fn effective_web_url(resolved: &ServerNamespace, env_lookup: &EnvLookup) -> String {
    env_lookup(EnvVars::FABRO_WEB_URL)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| resolved.web.url.clone())
}

fn canonical_origin_error(value: &str) -> String {
    format!(
        "server.web.url is required and must be an absolute http(s) URL (got \"{value}\"). Set it in your settings file or via the FABRO_WEB_URL environment variable."
    )
}
