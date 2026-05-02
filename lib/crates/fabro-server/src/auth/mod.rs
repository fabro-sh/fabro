mod browser_shell;
mod cli_flow;
mod github_endpoints;
mod jwt;
mod keys;
mod translate;

use strum::IntoStaticStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(crate) enum AuthErrorCode {
    Unauthorized,
    AccessTokenInvalid,
    AccessTokenExpired,
    InvalidRequest,
    InvalidCode,
    PkceVerificationFailed,
    RedirectUriMismatch,
}

impl AuthErrorCode {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        self.into()
    }
}

pub(crate) const REFRESH_TOKEN_PREFIX: &str = "fabro_refresh_";

pub(crate) use browser_shell::browser_shell;
pub(crate) use cli_flow::web_routes;
pub(crate) use fabro_store::{AuthCode, ConsumeOutcome, RefreshToken};
pub use github_endpoints::GithubEndpoints;
pub(crate) use jwt::{JwtError, JwtSubject, issue, verify};
pub(crate) use keys::{
    JwtSigningKey, KeyDeriveError, derive_cookie_key, derive_jwt_key, derive_worker_jwt_key,
};
pub(crate) use translate::{auth_translation_middleware, demo_routing_middleware};
