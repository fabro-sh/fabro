use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use chrono::{Duration, Utc};
use fabro_types::{IdpIdentity, RunAuthMethod};
use fabro_util::dev_token::validate_dev_token_format;
use tracing::trace;

use crate::auth::{self, JwtSubject};
use crate::jwt_auth::{AuthMode, ConfiguredAuth, dev_token_matches};
use crate::server::AppState;
use crate::web_auth::{self, SessionCookie};

const ACCESS_TOKEN_TTL_MINUTES: i64 = 10;
const DEV_IDP_ISSUER: &str = "fabro:dev";
const DEV_IDP_SUBJECT: &str = "dev";

pub(crate) async fn demo_routing_middleware(mut req: Request, next: Next) -> Response {
    let cookies = web_auth::parse_cookie_header(req.headers());
    if cookies
        .get("fabro-demo")
        .is_some_and(|cookie| cookie.value() == "1")
    {
        req.headers_mut()
            .insert("x-fabro-demo", HeaderValue::from_static("1"));
    }
    next.run(req).await
}

pub(crate) async fn auth_translation_middleware(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let translated = match req
        .extensions()
        .get::<AuthMode>()
        .expect("AuthMode extension must be added to the router")
    {
        AuthMode::Disabled => return next.run(req).await,
        AuthMode::Enabled(config) => {
            if let Some(auth_header) = req.headers().get(header::AUTHORIZATION) {
                auth_header
                    .to_str()
                    .ok()
                    .and_then(|value| value.strip_prefix("Bearer "))
                    .and_then(|token| translate_bearer_token(token, config))
            } else {
                translate_session_cookie(req.headers(), state.as_ref(), config)
            }
        }
    };

    if let Some(token) = translated {
        req.headers_mut()
            .insert(header::AUTHORIZATION, bearer_header_value(&token));
    }

    next.run(req).await
}

fn translate_bearer_token(token: &str, config: &ConfiguredAuth) -> Option<String> {
    if token.starts_with("fabro_refresh_") || !token.starts_with("fabro_dev_") {
        return None;
    }

    let expected = config.dev_token.as_deref()?;
    if !validate_dev_token_format(token) || !dev_token_matches(token, expected) {
        return None;
    }

    let jwt_key = config.jwt_key.as_ref()?;
    let jwt_issuer = config.jwt_issuer.as_deref()?;
    trace!(auth_method = "dev-token", "Translated dev token into JWT");
    Some(auth::issue(
        jwt_key,
        jwt_issuer,
        &JwtSubject {
            identity:    dev_identity(),
            login:       "dev".to_string(),
            name:        "Dev Token".to_string(),
            email:       "dev@fabro.local".to_string(),
            avatar_url:  String::new(),
            user_url:    String::new(),
            auth_method: RunAuthMethod::DevToken,
        },
        Duration::minutes(ACCESS_TOKEN_TTL_MINUTES),
    ))
}

fn translate_session_cookie(
    headers: &HeaderMap,
    state: &AppState,
    config: &ConfiguredAuth,
) -> Option<String> {
    let session_key = state.session_key()?;
    let session = web_auth::read_private_session(headers, &session_key)?;
    let jwt_key = config.jwt_key.as_ref()?;
    let jwt_issuer = config.jwt_issuer.as_deref()?;
    let identity = session
        .identity
        .clone()
        .or_else(|| legacy_dev_identity(&session))?;

    let ttl = session_ttl(&session)?;
    trace!(auth_method = ?session.auth_method, "Translated session cookie into JWT");
    Some(auth::issue(
        jwt_key,
        jwt_issuer,
        &JwtSubject {
            identity,
            login: session.login.clone(),
            name: session.name.clone(),
            email: session.email.clone(),
            avatar_url: session.avatar_url.clone(),
            user_url: session.user_url.clone(),
            auth_method: session.auth_method,
        },
        ttl,
    ))
}

fn session_ttl(session: &SessionCookie) -> Option<Duration> {
    let remaining_seconds = session.exp.saturating_sub(Utc::now().timestamp());
    let ttl_seconds =
        remaining_seconds.min(Duration::minutes(ACCESS_TOKEN_TTL_MINUTES).num_seconds());
    (ttl_seconds > 0).then_some(Duration::seconds(ttl_seconds))
}

fn legacy_dev_identity(session: &SessionCookie) -> Option<IdpIdentity> {
    (session.auth_method == RunAuthMethod::DevToken).then(dev_identity)
}

fn dev_identity() -> IdpIdentity {
    IdpIdentity::new(DEV_IDP_ISSUER, DEV_IDP_SUBJECT).expect("dev token identity should be valid")
}

fn bearer_header_value(token: &str) -> HeaderValue {
    HeaderValue::from_str(&format!("Bearer {token}"))
        .expect("minted JWT bearer header should be ASCII")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{HeaderMap, Request, StatusCode, header};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::{Json, Router, middleware};
    use cookie::{Cookie, CookieJar};
    use fabro_config::{RunLayer, ServerSettingsBuilder};
    use fabro_types::settings::ServerAuthMethod;
    use fabro_types::{IdpIdentity, RunAuthMethod};
    use serde_json::json;
    use tower::ServiceExt;

    use super::{auth_translation_middleware, demo_routing_middleware};
    use crate::auth::{self, JwtSigningKey};
    use crate::jwt_auth::{AuthMode, ConfiguredAuth};
    use crate::server::{self, RouterOptions};
    use crate::web_auth::{SESSION_COOKIE_NAME, SessionCookie};

    const SESSION_SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const DEV_TOKEN: &str =
        "fabro_dev_abababababababababababababababababababababababababababababababab";
    const WRONG_DEV_TOKEN: &str =
        "fabro_dev_cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

    async fn inspect_headers(headers: HeaderMap) -> impl IntoResponse {
        Json(json!({
            "authorization": headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            "demo": headers
                .get("x-fabro-demo")
                .and_then(|value| value.to_str().ok()),
        }))
    }

    fn signing_key() -> JwtSigningKey {
        auth::derive_jwt_key(SESSION_SECRET.as_bytes()).expect("jwt signing key should derive")
    }

    fn auth_mode() -> AuthMode {
        AuthMode::Enabled(ConfiguredAuth {
            methods:    vec![ServerAuthMethod::DevToken, ServerAuthMethod::Github],
            dev_token:  Some(DEV_TOKEN.to_string()),
            jwt_key:    Some(signing_key()),
            jwt_issuer: Some("https://fabro.example".to_string()),
        })
    }

    fn translation_router(state: Arc<server::AppState>) -> Router {
        Router::new()
            .route("/inspect", get(inspect_headers))
            .layer(middleware::from_fn_with_state(
                Arc::clone(&state),
                auth_translation_middleware,
            ))
            .layer(axum::Extension(auth_mode()))
            .with_state(state)
    }

    fn translation_router_without_auth_mode(state: Arc<server::AppState>) -> Router {
        Router::new()
            .route("/inspect", get(inspect_headers))
            .layer(middleware::from_fn_with_state(
                Arc::clone(&state),
                auth_translation_middleware,
            ))
            .with_state(state)
    }

    fn demo_router() -> Router {
        Router::new()
            .route("/inspect", get(inspect_headers))
            .layer(middleware::from_fn(demo_routing_middleware))
    }

    fn test_server_settings() -> fabro_types::ServerSettings {
        ServerSettingsBuilder::from_toml(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]
"#,
        )
        .expect("test settings should resolve")
    }

    fn test_state() -> Arc<server::AppState> {
        server::create_test_app_state_with_runtime_settings_and_session_key(
            test_server_settings(),
            RunLayer::default(),
            Some(SESSION_SECRET),
        )
    }

    fn session_cookie(session: &SessionCookie, state: &server::AppState) -> String {
        let key = state.session_key().expect("session key should exist");
        let mut jar = CookieJar::new();
        jar.private_mut(&key).add(Cookie::new(
            SESSION_COOKIE_NAME,
            serde_json::to_string(session).unwrap(),
        ));
        jar.delta()
            .next()
            .expect("private session cookie should exist")
            .encoded()
            .to_string()
    }

    fn github_session() -> SessionCookie {
        SessionCookie {
            v:           2,
            login:       "octocat".to_string(),
            auth_method: RunAuthMethod::Github,
            identity:    Some(IdpIdentity::new("https://github.com", "12345").unwrap()),
            name:        "The Octocat".to_string(),
            email:       "octocat@example.com".to_string(),
            avatar_url:  "https://example.com/octocat.png".to_string(),
            user_url:    "https://github.com/octocat".to_string(),
            iat:         chrono::Utc::now().timestamp(),
            exp:         (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp(),
        }
    }

    fn dev_session(identity: Option<IdpIdentity>) -> SessionCookie {
        SessionCookie {
            v: 2,
            login: "dev".to_string(),
            auth_method: RunAuthMethod::DevToken,
            identity,
            name: "Development User".to_string(),
            email: "dev@localhost".to_string(),
            avatar_url: "/images/logo.svg".to_string(),
            user_url: String::new(),
            iat: chrono::Utc::now().timestamp(),
            exp: (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp(),
        }
    }

    fn issue_github_jwt() -> String {
        auth::issue(
            &signing_key(),
            "https://fabro.example",
            &auth::JwtSubject {
                identity:    IdpIdentity::new("https://github.com", "12345").unwrap(),
                login:       "octocat".to_string(),
                name:        "The Octocat".to_string(),
                email:       "octocat@example.com".to_string(),
                avatar_url:  "https://example.com/octocat.png".to_string(),
                user_url:    "https://github.com/octocat".to_string(),
                auth_method: RunAuthMethod::Github,
            },
            chrono::Duration::minutes(10),
        )
    }

    macro_rules! response_json {
        ($response:expr) => {
            fabro_test::expect_axum_json($response, StatusCode::OK, concat!(file!(), ":", line!()))
        };
    }

    macro_rules! assert_status {
        ($response:expr, $expected:expr) => {
            fabro_test::assert_axum_status($response, $expected, concat!(file!(), ":", line!()))
        };
    }

    #[tokio::test]
    async fn demo_routing_middleware_sets_demo_header_from_cookie() {
        let app = demo_router();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/inspect")
                    .header(header::COOKIE, "fabro-demo=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let json = response_json!(response).await;
        assert_eq!(json["demo"], "1");
    }

    #[tokio::test]
    async fn auth_translation_passes_existing_bearer_through_unchanged() {
        let state = test_state();
        let token = issue_github_jwt();
        let response = translation_router(state)
            .oneshot(
                Request::builder()
                    .uri("/inspect")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let json = response_json!(response).await;
        assert_eq!(json["authorization"], format!("Bearer {token}"));
    }

    #[tokio::test]
    async fn auth_translation_replaces_valid_dev_token_with_jwt() {
        let state = test_state();
        let response = translation_router(state)
            .oneshot(
                Request::builder()
                    .uri("/inspect")
                    .header(header::AUTHORIZATION, format!("Bearer {DEV_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let json = response_json!(response).await;
        let auth_header = json["authorization"]
            .as_str()
            .expect("authorization header should be present");
        let token = auth_header
            .strip_prefix("Bearer ")
            .expect("authorization should be a bearer token");
        let claims = auth::verify(&signing_key(), "https://fabro.example", token).unwrap();
        assert_eq!(claims.login, "dev");
        assert_eq!(claims.name, "Dev Token");
        assert_eq!(claims.email, "dev@fabro.local");
        assert_eq!(claims.idp_issuer, "fabro:dev");
        assert_eq!(claims.idp_subject, "dev");
        assert_eq!(claims.auth_method, RunAuthMethod::DevToken);
    }

    #[tokio::test]
    async fn auth_translation_mints_jwt_from_github_session_cookie() {
        let state = test_state();
        let cookie = session_cookie(&github_session(), state.as_ref());
        let response = translation_router(Arc::clone(&state))
            .oneshot(
                Request::builder()
                    .uri("/inspect")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let json = response_json!(response).await;
        let token = json["authorization"]
            .as_str()
            .and_then(|value| value.strip_prefix("Bearer "))
            .expect("authorization should be minted");
        let claims = auth::verify(&signing_key(), "https://fabro.example", token).unwrap();
        assert_eq!(claims.login, "octocat");
        assert_eq!(claims.name, "The Octocat");
        assert_eq!(claims.email, "octocat@example.com");
        assert_eq!(claims.idp_issuer, "https://github.com");
        assert_eq!(claims.idp_subject, "12345");
        assert_eq!(claims.avatar_url, "https://example.com/octocat.png");
        assert_eq!(claims.user_url, "https://github.com/octocat");
        assert_eq!(claims.auth_method, RunAuthMethod::Github);
    }

    #[tokio::test]
    async fn auth_translation_applies_legacy_dev_session_identity_fallback() {
        let state = test_state();
        let cookie = session_cookie(&dev_session(None), state.as_ref());
        let response = translation_router(Arc::clone(&state))
            .oneshot(
                Request::builder()
                    .uri("/inspect")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let json = response_json!(response).await;
        let token = json["authorization"]
            .as_str()
            .and_then(|value| value.strip_prefix("Bearer "))
            .expect("authorization should be minted");
        let claims = auth::verify(&signing_key(), "https://fabro.example", token).unwrap();
        assert_eq!(claims.idp_issuer, "fabro:dev");
        assert_eq!(claims.idp_subject, "dev");
        assert_eq!(claims.auth_method, RunAuthMethod::DevToken);
    }

    #[tokio::test]
    async fn auth_translation_does_not_mint_for_demo_header_alone() {
        let state = test_state();
        let response = translation_router(state)
            .oneshot(
                Request::builder()
                    .uri("/inspect")
                    .header("x-fabro-demo", "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let json = response_json!(response).await;
        assert_eq!(json["authorization"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn auth_translation_prefers_existing_authorization_over_session_cookie() {
        let state = test_state();
        let token = issue_github_jwt();
        let cookie = session_cookie(&github_session(), state.as_ref());
        let response = translation_router(Arc::clone(&state))
            .oneshot(
                Request::builder()
                    .uri("/inspect")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let json = response_json!(response).await;
        assert_eq!(json["authorization"], format!("Bearer {token}"));
    }

    #[tokio::test]
    async fn auth_translation_leaves_invalid_dev_token_unchanged() {
        let state = test_state();
        let response = translation_router(state)
            .oneshot(
                Request::builder()
                    .uri("/inspect")
                    .header(header::AUTHORIZATION, format!("Bearer {WRONG_DEV_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let json = response_json!(response).await;
        assert_eq!(json["authorization"], format!("Bearer {WRONG_DEV_TOKEN}"));
    }

    #[tokio::test]
    async fn auth_translation_passes_refresh_token_through_unchanged() {
        let state = test_state();
        let refresh_token = "fabro_refresh_abcdefghijklmnop";
        let response = translation_router(state)
            .oneshot(
                Request::builder()
                    .uri("/inspect")
                    .header(header::AUTHORIZATION, format!("Bearer {refresh_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let json = response_json!(response).await;
        assert_eq!(json["authorization"], format!("Bearer {refresh_token}"));
    }

    #[tokio::test]
    async fn auth_translation_mints_from_session_even_when_demo_header_set() {
        let state = test_state();
        let cookie = session_cookie(&github_session(), state.as_ref());
        let response = translation_router(Arc::clone(&state))
            .oneshot(
                Request::builder()
                    .uri("/inspect")
                    .header(header::COOKIE, cookie)
                    .header("x-fabro-demo", "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let json = response_json!(response).await;
        let token = json["authorization"]
            .as_str()
            .and_then(|value| value.strip_prefix("Bearer "))
            .expect("authorization should be minted despite demo header");
        let claims = auth::verify(&signing_key(), "https://fabro.example", token).unwrap();
        assert_eq!(claims.login, "octocat");
        assert_eq!(claims.auth_method, RunAuthMethod::Github);
        assert_eq!(json["demo"], "1");
    }

    #[tokio::test]
    async fn auth_translation_panics_without_auth_mode_extension() {
        let state = test_state();
        let handle = tokio::spawn(async move {
            let _ = translation_router_without_auth_mode(state)
                .oneshot(
                    Request::builder()
                        .uri("/inspect")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await;
        });
        let err = handle.await.expect_err("request should panic");
        assert!(err.is_panic());
    }

    #[tokio::test]
    async fn full_router_rejects_demo_cookie_without_credentials() {
        let state = test_state();
        let app = server::build_router_with_options(
            state,
            &auth_mode(),
            Arc::new(crate::ip_allowlist::IpAllowlistConfig::default()),
            RouterOptions::default(),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/health/diagnostics")
                    .header(header::COOKIE, "fabro-demo=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_status!(response, StatusCode::UNAUTHORIZED).await;
    }

    #[tokio::test]
    async fn full_router_routes_demo_cookie_to_demo_handler_with_session() {
        let state = test_state();
        let cookie = session_cookie(&github_session(), state.as_ref());
        let app = server::build_router_with_options(
            Arc::clone(&state),
            &auth_mode(),
            Arc::new(crate::ip_allowlist::IpAllowlistConfig::default()),
            RouterOptions::default(),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/health/diagnostics")
                    .header(header::COOKIE, format!("{cookie}; fabro-demo=1"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = response_json!(response).await;
        assert_eq!(
            body["sections"][0]["checks"][0]["summary"], "demo configured",
            "demo handler should have served /health/diagnostics",
        );
    }

    #[tokio::test]
    async fn full_router_does_not_demo_dispatch_auth_routes() {
        let state = test_state();
        let app = server::build_router_with_options(
            state,
            &auth_mode(),
            Arc::new(crate::ip_allowlist::IpAllowlistConfig::default()),
            RouterOptions::default(),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/auth/login/github")
                    .header(header::COOKIE, "fabro-demo=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_ne!(
            response.status(),
            StatusCode::NOT_FOUND,
            "demo cookie must not steal /auth/* routes from the real router",
        );
    }

    #[tokio::test]
    async fn full_router_accepts_dev_token_bearer_when_web_is_disabled() {
        let state = test_state();
        let app = server::build_router_with_options(
            state,
            &auth_mode(),
            Arc::new(crate::ip_allowlist::IpAllowlistConfig::default()),
            RouterOptions {
                web_enabled:                 false,
                github_endpoints:            None,
                github_webhook_ip_allowlist: None,
                static_asset_root:           None,
            },
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/health/diagnostics")
                    .header(header::AUTHORIZATION, format!("Bearer {DEV_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_status!(response, StatusCode::OK).await;
    }
}
