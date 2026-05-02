#[cfg(test)]
use std::sync::Mutex;
use std::sync::{Arc, OnceLock};

use axum::extract::Request;
#[cfg(test)]
use axum::extract::State as AxumState;
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use axum::{Router, middleware};
use chrono::Duration;
use fabro_types::settings::ServerAuthMethod;
use fabro_types::{AuthMethod, IdpIdentity};

use crate::auth;
use crate::ip_allowlist::IpAllowlistConfig;
use crate::jwt_auth::{AuthMode, ConfiguredAuth};
#[cfg(test)]
use crate::principal_middleware::{AuthContextSlot, RequestAuthContext};
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
        static BEARER: OnceLock<HeaderValue> = OnceLock::new();
        let bearer = BEARER.get_or_init(|| {
            HeaderValue::from_str(&format!("Bearer {}", issue_test_user_token()))
                .expect("test JWT bearer header is valid")
        });
        req.headers_mut()
            .insert(header::AUTHORIZATION, bearer.clone());
    }
    next.run(req).await
}

fn issue_test_user_token() -> String {
    let key = auth::derive_jwt_key(TEST_SESSION_SECRET.as_bytes())
        .expect("test jwt signing key should derive");
    auth::issue(
        &key,
        "https://fabro.test",
        &auth::JwtSubject {
            identity:    IdpIdentity::new("fabro:dev", "dev")
                .expect("test identity should be valid"),
            login:       "dev".to_string(),
            name:        "Dev Token".to_string(),
            email:       "dev@fabro.local".to_string(),
            avatar_url:  String::new(),
            user_url:    String::new(),
            auth_method: AuthMethod::DevToken,
        },
        Duration::days(3650),
    )
}

#[cfg(test)]
pub(crate) async fn capture_auth_context(
    AxumState(captured): AxumState<Arc<Mutex<Vec<RequestAuthContext>>>>,
    mut req: Request,
    next: Next,
) -> Response {
    let slot = AuthContextSlot::initial();
    req.extensions_mut().insert(slot.clone());
    let response = next.run(req).await;
    captured
        .lock()
        .expect("captured auth contexts lock poisoned")
        .push(slot.snapshot());
    response
}
