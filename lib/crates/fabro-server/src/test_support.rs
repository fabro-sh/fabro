use std::sync::Arc;

use axum::extract::Request;
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use axum::{Router, middleware};
use fabro_types::settings::ServerAuthMethod;

use crate::auth;
use crate::ip_allowlist::IpAllowlistConfig;
use crate::jwt_auth::{AuthMode, ConfiguredAuth};
use crate::server::{self, AppState, RouterOptions};

pub const TEST_DEV_TOKEN: &str =
    "fabro_dev_abababababababababababababababababababababababababababababababab";
pub const TEST_SESSION_SECRET: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[doc(hidden)]
#[must_use]
pub fn test_auth_mode() -> AuthMode {
    AuthMode::Enabled(ConfiguredAuth {
        methods:    vec![ServerAuthMethod::DevToken, ServerAuthMethod::Github],
        dev_token:  Some(TEST_DEV_TOKEN.to_string()),
        jwt_key:    Some(
            auth::derive_jwt_key(TEST_SESSION_SECRET.as_bytes())
                .expect("test jwt signing key should derive"),
        ),
        jwt_issuer: Some("https://fabro.test".to_string()),
    })
}

#[doc(hidden)]
pub fn build_test_router(state: Arc<AppState>) -> Router {
    with_test_user(server::build_router(state, test_auth_mode()))
}

#[doc(hidden)]
pub fn build_test_router_with_options(
    state: Arc<AppState>,
    ip_allowlist_config: Arc<IpAllowlistConfig>,
    options: RouterOptions,
) -> Router {
    with_test_user(server::build_router_with_options(
        state,
        &test_auth_mode(),
        ip_allowlist_config,
        options,
    ))
}

#[doc(hidden)]
pub fn with_test_user(router: Router) -> Router {
    router.layer(middleware::from_fn(inject_test_user_bearer))
}

async fn inject_test_user_bearer(mut req: Request, next: Next) -> Response {
    if req.uri().path().starts_with("/api/") && !req.headers().contains_key(header::AUTHORIZATION) {
        req.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static(
                "Bearer fabro_dev_abababababababababababababababababababababababababababababababab",
            ),
        );
    }
    next.run(req).await
}
