use anyhow::{Result, anyhow};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use fabro_types::RunAuthMethod;
use fabro_types::settings::{ServerAuthMethod, ServerSettings as ResolvedServerSettings};
use fabro_util::dev_token::validate_dev_token_format;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::error::ApiError;
use crate::web_auth::SessionCookie;

type HmacSha256 = Hmac<Sha256>;
const DEV_TOKEN_COMPARE_KEY: &[u8] = b"fabro-dev-token-compare-key";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialSource {
    AuthorizationHeader,
    SessionCookie,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedAuth {
    pub login:             String,
    pub auth_method:       RunAuthMethod,
    pub credential_source: CredentialSource,
    pub provider_id:       Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfiguredAuth {
    pub methods:   Vec<ServerAuthMethod>,
    pub dev_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthMode {
    Enabled(ConfiguredAuth),
    Disabled,
}

pub fn resolve_auth_mode(settings: &ResolvedServerSettings) -> Result<AuthMode> {
    resolve_auth_mode_with_lookup(settings, |name| std::env::var(name).ok())
}

pub fn resolve_auth_mode_with_lookup<F>(
    settings: &ResolvedServerSettings,
    lookup: F,
) -> Result<AuthMode>
where
    F: Fn(&str) -> Option<String>,
{
    let methods = settings.auth.methods.clone();
    if methods.is_empty() {
        return Err(anyhow!(
            "Fabro server refuses to start: server.auth.methods must not be empty."
        ));
    }

    let web_enabled = settings.web.enabled;
    if web_enabled && lookup("SESSION_SECRET").is_none() {
        return Err(anyhow!(
            "Fabro server refuses to start: web UI is enabled but SESSION_SECRET is not set."
        ));
    }

    let dev_token = if methods.contains(&ServerAuthMethod::DevToken) {
        let token = lookup("FABRO_DEV_TOKEN").ok_or_else(|| {
            anyhow!(
                "Fabro server refuses to start: dev-token auth is enabled but FABRO_DEV_TOKEN is not set."
            )
        })?;
        if !validate_dev_token_format(&token) {
            return Err(anyhow!(
                "Fabro server refuses to start: FABRO_DEV_TOKEN has invalid format."
            ));
        }
        Some(token)
    } else {
        None
    };

    if methods.contains(&ServerAuthMethod::Github) {
        if settings.integrations.github.client_id.is_none() {
            return Err(anyhow!(
                "Fabro server refuses to start: github auth is enabled but server.integrations.github.client_id is not configured."
            ));
        }
        if lookup("GITHUB_APP_CLIENT_SECRET").is_none() {
            return Err(anyhow!(
                "Fabro server refuses to start: github auth is enabled but GITHUB_APP_CLIENT_SECRET is not set."
            ));
        }
    }

    Ok(AuthMode::Enabled(ConfiguredAuth { methods, dev_token }))
}

pub(crate) fn dev_token_matches(provided: &str, expected: &str) -> bool {
    let Ok(mut provided_mac) = HmacSha256::new_from_slice(DEV_TOKEN_COMPARE_KEY) else {
        return false;
    };
    provided_mac.update(provided.as_bytes());
    let provided_mac = provided_mac.finalize().into_bytes();

    let Ok(mut expected_mac) = HmacSha256::new_from_slice(DEV_TOKEN_COMPARE_KEY) else {
        return false;
    };
    expected_mac.update(expected.as_bytes());
    expected_mac.verify_slice(&provided_mac).is_ok()
}

fn config_allows_run_auth_method(config: &ConfiguredAuth, method: RunAuthMethod) -> bool {
    match method {
        RunAuthMethod::Disabled => false,
        RunAuthMethod::DevToken => config.methods.contains(&ServerAuthMethod::DevToken),
        RunAuthMethod::Github => config.methods.contains(&ServerAuthMethod::Github),
    }
}

fn bearer_token(parts: &Parts) -> Option<Result<&str, ApiError>> {
    let header = parts.headers.get("authorization")?;
    let Ok(header) = header.to_str() else {
        return Some(Err(ApiError::unauthorized()));
    };
    Some(
        header
            .strip_prefix("Bearer ")
            .ok_or_else(ApiError::unauthorized),
    )
}

fn authenticate_bearer(token: &str, config: &ConfiguredAuth) -> Result<VerifiedAuth, ApiError> {
    let Some(expected) = config.dev_token.as_deref() else {
        return Err(ApiError::unauthorized());
    };
    if !validate_dev_token_format(token) || !dev_token_matches(token, expected) {
        return Err(ApiError::unauthorized());
    }
    Ok(VerifiedAuth {
        login:             "dev".to_string(),
        auth_method:       RunAuthMethod::DevToken,
        credential_source: CredentialSource::AuthorizationHeader,
        provider_id:       None,
    })
}

fn authenticate_session(parts: &Parts, config: &ConfiguredAuth) -> Result<VerifiedAuth, ApiError> {
    let Some(session) = parts.extensions.get::<SessionCookie>() else {
        return Err(ApiError::unauthorized());
    };
    if !config_allows_run_auth_method(config, session.auth_method) {
        return Err(ApiError::unauthorized());
    }
    Ok(VerifiedAuth {
        login:             session.login.clone(),
        auth_method:       session.auth_method,
        credential_source: CredentialSource::SessionCookie,
        provider_id:       session.provider_id,
    })
}

fn authenticate_parts(parts: &Parts) -> Result<Option<VerifiedAuth>, ApiError> {
    let auth_mode = parts
        .extensions
        .get::<AuthMode>()
        .expect("AuthMode extension must be added to the router");

    let AuthMode::Enabled(config) = auth_mode else {
        return Ok(None);
    };

    if let Some(token) = bearer_token(parts) {
        return authenticate_bearer(token?, config).map(Some);
    }

    authenticate_session(parts, config).map(Some)
}

/// Axum extractor that enforces authentication on a route.
pub struct AuthenticatedService;

pub fn authenticate_service_parts(parts: &Parts) -> Result<(), ApiError> {
    authenticate_parts(parts).map(|_| ())
}

impl<S: Send + Sync> FromRequestParts<S> for AuthenticatedService {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        authenticate_service_parts(parts)?;
        Ok(Self)
    }
}

/// Axum extractor that authenticates and extracts the request subject.
pub struct AuthenticatedSubject {
    pub login:       Option<String>,
    pub auth_method: RunAuthMethod,
}

impl<S: Send + Sync> FromRequestParts<S> for AuthenticatedSubject {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let auth_mode = parts
            .extensions
            .get::<AuthMode>()
            .expect("AuthMode extension must be added to the router");

        match auth_mode {
            AuthMode::Disabled => Ok(Self {
                login:       None,
                auth_method: RunAuthMethod::Disabled,
            }),
            AuthMode::Enabled(config) => {
                let auth = if let Some(token) = bearer_token(parts) {
                    authenticate_bearer(token?, config)?
                } else {
                    authenticate_session(parts, config)?
                };
                Ok(Self {
                    login:       Some(auth.login),
                    auth_method: auth.auth_method,
                })
            }
        }
    }
}

pub fn auth_method_name(method: ServerAuthMethod) -> &'static str {
    match method {
        ServerAuthMethod::DevToken => "dev-token",
        ServerAuthMethod::Github => "github",
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::{Json, Router};
    use cookie::Key;
    use fabro_config::{parse_settings_layer, resolve_server_from_file};
    use fabro_types::settings::ServerAuthMethod;
    use tower::ServiceExt;

    use super::*;
    use crate::web_auth::SessionCookie;

    fn settings(source: &str) -> ResolvedServerSettings {
        let file = parse_settings_layer(source).expect("fixture should parse");
        resolve_server_from_file(&file).expect("fixture should resolve")
    }

    fn empty_lookup(_name: &str) -> Option<String> {
        None
    }

    fn make_session(auth_method: RunAuthMethod) -> SessionCookie {
        SessionCookie {
            v: 1,
            login: "alice".to_string(),
            auth_method,
            provider_id: Some(123),
            name: "Alice".to_string(),
            email: "alice@example.com".to_string(),
            avatar_url: "https://example.com/alice.png".to_string(),
            user_url: "https://github.com/alice".to_string(),
            iat: chrono::Utc::now().timestamp(),
            exp: (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp(),
        }
    }

    async fn protected_handler(_auth: AuthenticatedService) -> impl IntoResponse {
        "ok"
    }

    async fn subject_handler(subject: AuthenticatedSubject) -> impl IntoResponse {
        Json(serde_json::json!({
            "login": subject.login,
            "auth_method": subject.auth_method,
        }))
    }

    fn test_router(mode: AuthMode) -> Router {
        Router::new()
            .route("/test", get(protected_handler))
            .layer(axum::Extension(mode))
    }

    fn subject_router(mode: AuthMode) -> Router {
        Router::new()
            .route("/subject", get(subject_handler))
            .layer(axum::Extension(mode))
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

    fn dev_token_mode() -> AuthMode {
        AuthMode::Enabled(ConfiguredAuth {
            methods:   vec![ServerAuthMethod::DevToken],
            dev_token: Some(
                "fabro_dev_abababababababababababababababababababababababababababababababab"
                    .to_string(),
            ),
        })
    }

    #[test]
    fn fails_when_auth_methods_empty() {
        let file = parse_settings_layer(
            r"
_version = 1

[server.auth]
methods = []
",
        )
        .expect("fixture should parse");
        let errors = resolve_server_from_file(&file).expect_err("empty auth methods should fail");
        assert!(errors.iter().any(|err| matches!(
            err,
            fabro_config::resolve::ResolveError::Invalid { path, reason }
                if path == "server.auth.methods" && reason.contains("must not be empty")
        )));
    }

    #[test]
    fn fails_when_web_enabled_without_session_secret() {
        let file = settings("_version = 1\n");
        let err = resolve_auth_mode_with_lookup(&file, empty_lookup)
            .expect_err("missing session secret should fail");
        assert!(err.to_string().contains("SESSION_SECRET"));
    }

    #[test]
    fn resolves_dev_token_mode_when_secrets_present() {
        let file = settings("_version = 1\n");
        let mode = resolve_auth_mode_with_lookup(&file, |name| match name {
            "SESSION_SECRET" => {
                Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string())
            }
            "FABRO_DEV_TOKEN" => Some(
                "fabro_dev_abababababababababababababababababababababababababababababababab"
                    .to_string(),
            ),
            _ => None,
        })
        .expect("dev-token auth should resolve");
        let AuthMode::Enabled(config) = mode else {
            panic!("expected enabled mode");
        };
        assert_eq!(config.methods, vec![ServerAuthMethod::DevToken]);
        assert!(config.dev_token.is_some());
    }

    #[test]
    fn fails_when_github_enabled_without_client_secret() {
        let file = settings(
            r#"
_version = 1

[server.auth]
methods = ["github"]

[server.auth.github]
allowed_usernames = ["alice"]

[server.integrations.github]
client_id = "Iv1.test"
"#,
        );
        let err = resolve_auth_mode_with_lookup(&file, |name| {
            (name == "SESSION_SECRET").then(|| {
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string()
            })
        })
        .expect_err("github auth should require client secret");
        assert!(err.to_string().contains("GITHUB_APP_CLIENT_SECRET"));
    }

    #[tokio::test]
    async fn disabled_mode_allows_request() {
        let app = test_router(AuthMode::Disabled);
        let response = app
            .oneshot(Request::builder().uri("/test").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_status!(response, StatusCode::OK).await;
    }

    #[tokio::test]
    async fn rejects_missing_credentials() {
        let app = test_router(dev_token_mode());
        let response = app
            .oneshot(Request::builder().uri("/test").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_status!(response, StatusCode::UNAUTHORIZED).await;
    }

    #[tokio::test]
    async fn accepts_valid_dev_token_bearer() {
        let app = subject_router(dev_token_mode());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/subject")
                    .header(
                        "authorization",
                        "Bearer fabro_dev_abababababababababababababababababababababababababababababababab",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let json = response_json!(response).await;
        assert_eq!(json["login"], "dev");
        assert_eq!(json["auth_method"], "dev_token");
    }

    #[tokio::test]
    async fn invalid_authorization_header_does_not_fall_back_to_cookie() {
        let app = test_router(dev_token_mode());
        let key =
            Key::derive_from(b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        let session = make_session(RunAuthMethod::DevToken);
        let mut jar = cookie::CookieJar::new();
        jar.private_mut(&key).add(cookie::Cookie::new(
            crate::web_auth::SESSION_COOKIE_NAME,
            serde_json::to_string(&session).unwrap(),
        ));
        let cookie = jar
            .delta()
            .next()
            .expect("private cookie should exist")
            .encoded()
            .to_string();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("authorization", "Basic nope")
                    .header("cookie", cookie)
                    .extension(session)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_status!(response, StatusCode::UNAUTHORIZED).await;
    }

    #[tokio::test]
    async fn cookie_session_reports_github_provenance() {
        let app = subject_router(AuthMode::Enabled(ConfiguredAuth {
            methods:   vec![ServerAuthMethod::Github],
            dev_token: None,
        }));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/subject")
                    .extension(make_session(RunAuthMethod::Github))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let json = response_json!(response).await;
        assert_eq!(json["login"], "alice");
        assert_eq!(json["auth_method"], "github");
    }
}
