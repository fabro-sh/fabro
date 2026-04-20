use anyhow::{Result, anyhow};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use fabro_types::settings::{ServerAuthMethod, ServerSettings as ResolvedServerSettings};
use fabro_types::{IdpIdentity, RunAuthMethod};
use fabro_util::dev_token::validate_dev_token_format;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tracing::info;

use crate::auth::{JwtError, JwtSigningKey, KeyDeriveError, derive_cookie_key, derive_jwt_key};
use crate::error::ApiError;
use crate::web_auth::SessionCookie;

type HmacSha256 = Hmac<Sha256>;
const DEV_TOKEN_COMPARE_KEY: &[u8] = b"fabro-dev-token-compare-key";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialSource {
    AuthorizationHeader,
    JwtAccessToken,
    SessionCookie,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedAuth {
    pub login:             String,
    pub auth_method:       RunAuthMethod,
    pub credential_source: CredentialSource,
    pub identity:          Option<IdpIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfiguredAuth {
    pub(crate) methods:    Vec<ServerAuthMethod>,
    pub(crate) dev_token:  Option<String>,
    pub(crate) jwt_key:    Option<JwtSigningKey>,
    pub(crate) jwt_issuer: Option<String>,
}

impl ConfiguredAuth {
    pub fn new(methods: Vec<ServerAuthMethod>, dev_token: Option<String>) -> Self {
        Self {
            methods,
            dev_token,
            jwt_key: None,
            jwt_issuer: None,
        }
    }
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
    let github_enabled = methods.contains(&ServerAuthMethod::Github);
    if methods.is_empty() {
        return Err(anyhow!(
            "Fabro server refuses to start: server.auth.methods must not be empty."
        ));
    }

    let web_enabled = settings.web.enabled;
    let session_secret = lookup("SESSION_SECRET");
    if web_enabled {
        let secret = session_secret.as_deref().ok_or_else(|| {
            anyhow!(
                "Fabro server refuses to start: web UI is enabled but SESSION_SECRET is not set."
            )
        })?;
        derive_cookie_key(secret.as_bytes()).map_err(cookie_key_error)?;
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

    let (jwt_key, jwt_issuer) = if github_enabled {
        if !web_enabled {
            return Err(anyhow!(
                "Fabro server refuses to start: github auth is enabled but server.web.enabled is false."
            ));
        }
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
        let secret = session_secret
            .as_deref()
            .expect("web-enabled github auth should already require SESSION_SECRET");
        (
            Some(derive_jwt_key(secret.as_bytes()).map_err(jwt_key_error)?),
            resolve_jwt_issuer(settings, &lookup),
        )
    } else {
        (None, None)
    };

    Ok(AuthMode::Enabled(ConfiguredAuth {
        methods,
        dev_token,
        jwt_key,
        jwt_issuer,
    }))
}

fn resolve_jwt_issuer<F>(settings: &ResolvedServerSettings, lookup: &F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    settings
        .web
        .url
        .resolve(|name| lookup(name))
        .ok()
        .map(|resolved| resolved.value)
        .filter(|value| !value.is_empty())
}

fn cookie_key_error(err: KeyDeriveError) -> anyhow::Error {
    match err {
        KeyDeriveError::Empty => {
            anyhow!(
                "Fabro server refuses to start: web UI is enabled but SESSION_SECRET is not set."
            )
        }
        KeyDeriveError::TooShort {
            got_bytes,
            min_bytes,
        } => anyhow!(
            "Fabro server refuses to start: SESSION_SECRET must be at least {min_bytes} bytes (64 hex characters) when web UI is enabled. Current length: {got_bytes} bytes."
        ),
    }
}

fn jwt_key_error(err: KeyDeriveError) -> anyhow::Error {
    match err {
        KeyDeriveError::Empty => {
            anyhow!(
                "Fabro server refuses to start: github auth is enabled but SESSION_SECRET is not set."
            )
        }
        KeyDeriveError::TooShort {
            got_bytes,
            min_bytes,
        } => anyhow!(
            "Fabro server refuses to start: SESSION_SECRET must be at least {min_bytes} bytes (64 hex characters) when github auth is enabled - it now signs JWTs as well as session cookies. Current length: {got_bytes} bytes."
        ),
    }
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

fn authenticate_dev_token_bearer(
    token: &str,
    config: &ConfiguredAuth,
) -> Result<VerifiedAuth, ApiError> {
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
        identity:          None,
    })
}

fn authenticate_jwt_bearer(token: &str, config: &ConfiguredAuth) -> Result<VerifiedAuth, ApiError> {
    let Some(jwt_key) = config.jwt_key.as_ref() else {
        return Err(ApiError::unauthorized());
    };
    let Some(jwt_issuer) = config.jwt_issuer.as_deref() else {
        return Err(ApiError::unauthorized());
    };
    if !looks_like_jwt(token) {
        return Err(ApiError::unauthorized());
    }

    let claims = match crate::auth::verify(jwt_key, jwt_issuer, token) {
        Ok(claims) => claims,
        Err(JwtError::AccessTokenExpired) => {
            return Err(ApiError::unauthorized_with_code(
                "Authentication required.",
                "access_token_expired",
            ));
        }
        Err(JwtError::AccessTokenInvalid) => {
            return Err(ApiError::unauthorized_with_code(
                "Authentication required.",
                "access_token_invalid",
            ));
        }
    };

    if !config_allows_run_auth_method(config, claims.auth_method) {
        return Err(ApiError::unauthorized_with_code(
            "Authentication required.",
            "access_token_invalid",
        ));
    }

    let identity = IdpIdentity::new(&claims.idp_issuer, &claims.idp_subject).map_err(|_| {
        ApiError::unauthorized_with_code("Authentication required.", "access_token_invalid")
    })?;

    Ok(VerifiedAuth {
        login:             claims.login,
        auth_method:       claims.auth_method,
        credential_source: CredentialSource::JwtAccessToken,
        identity:          Some(identity),
    })
}

fn authenticate_bearer(
    parts: &Parts,
    token: &str,
    config: &ConfiguredAuth,
) -> Result<VerifiedAuth, ApiError> {
    if token.starts_with("fabro_dev_") {
        return authenticate_dev_token_bearer(token, config);
    }
    if token.starts_with("fabro_refresh_") {
        info!(
            path = %parts.uri.path(),
            "Refresh token presented at protected endpoint"
        );
        return Err(ApiError::unauthorized_with_code(
            "Authentication required.",
            "unauthorized",
        ));
    }

    authenticate_jwt_bearer(token, config)
}

fn looks_like_jwt(token: &str) -> bool {
    let mut segments = token.split('.');
    matches!(
        (
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next()
        ),
        (Some(header), Some(payload), Some(signature), None)
            if !header.is_empty() && !payload.is_empty() && !signature.is_empty()
    )
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
        identity:          session.identity.clone(),
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
        return authenticate_bearer(parts, token?, config).map(Some);
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
                    authenticate_bearer(parts, token?, config)?
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
    use std::sync::{Arc, Mutex as StdMutex};

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::{Json, Router};
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use cookie::Key;
    use fabro_config::{parse_settings_layer, resolve_server_from_file};
    use fabro_types::IdpIdentity;
    use fabro_types::settings::ServerAuthMethod;
    use tower::ServiceExt;
    use tracing::field::{Field, Visit};
    use tracing::{Event, Subscriber, subscriber};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::{Layer, Registry};

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
            v: 2,
            login: "alice".to_string(),
            auth_method,
            identity: (auth_method == RunAuthMethod::Github)
                .then(|| IdpIdentity::new("https://github.com", "123").unwrap()),
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

    async fn error_json(err: ApiError) -> serde_json::Value {
        let response = err.into_response();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn dev_token_mode() -> AuthMode {
        AuthMode::Enabled(ConfiguredAuth {
            methods:    vec![ServerAuthMethod::DevToken],
            dev_token:  Some(
                "fabro_dev_abababababababababababababababababababababababababababababababab"
                    .to_string(),
            ),
            jwt_key:    None,
            jwt_issuer: None,
        })
    }

    fn github_jwt_mode() -> AuthMode {
        AuthMode::Enabled(ConfiguredAuth {
            methods:    vec![ServerAuthMethod::Github],
            dev_token:  None,
            jwt_key:    Some(signing_key()),
            jwt_issuer: Some("https://fabro.example".to_string()),
        })
    }

    fn signing_key() -> JwtSigningKey {
        derive_jwt_key(b"0123456789abcdef0123456789abcdef").expect("jwt signing key should derive")
    }

    fn other_signing_key() -> JwtSigningKey {
        derive_jwt_key(b"fedcba9876543210fedcba9876543210").expect("jwt signing key should derive")
    }

    fn jwt_subject() -> crate::auth::JwtSubject {
        crate::auth::JwtSubject {
            identity:    IdpIdentity::new("https://github.com", "12345").unwrap(),
            login:       "octocat".to_string(),
            name:        "The Octocat".to_string(),
            email:       "octocat@example.com".to_string(),
            auth_method: RunAuthMethod::Github,
        }
    }

    fn issue_github_token(ttl: chrono::Duration) -> String {
        crate::auth::issue(&signing_key(), "https://fabro.example", &jwt_subject(), ttl)
    }

    fn request_parts(mode: AuthMode, request: Request<Body>) -> Parts {
        let (mut parts, _body) = request.into_parts();
        parts.extensions.insert(mode);
        parts
    }

    #[derive(Debug)]
    struct LogCapture {
        target: String,
        fields: Vec<(String, String)>,
    }

    #[derive(Default)]
    struct LogCaptureVisitor {
        fields: Vec<(String, String)>,
    }

    impl Visit for LogCaptureVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.fields
                .push((field.name().to_string(), format!("{value:?}")));
        }
    }

    struct LogCaptureLayer {
        events: Arc<StdMutex<Vec<LogCapture>>>,
    }

    impl<S: Subscriber> Layer<S> for LogCaptureLayer {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            if !event
                .metadata()
                .target()
                .starts_with("fabro_server::jwt_auth")
            {
                return;
            }

            let mut visitor = LogCaptureVisitor::default();
            event.record(&mut visitor);
            self.events.lock().unwrap().push(LogCapture {
                target: event.metadata().target().to_string(),
                fields: visitor.fields,
            });
        }
    }

    fn capture_logs<T>(f: impl FnOnce() -> T) -> (T, Arc<StdMutex<Vec<LogCapture>>>) {
        let events = Arc::new(StdMutex::new(Vec::<LogCapture>::new()));
        let layer = LogCaptureLayer {
            events: Arc::clone(&events),
        };
        let subscriber = Registry::default().with(layer);
        let result = subscriber::with_default(subscriber, f);
        (result, events)
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
        assert!(config.jwt_key.is_none());
        assert!(config.jwt_issuer.is_none());
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

    #[test]
    fn fails_when_github_enabled_without_web() {
        let file = settings(
            r#"
_version = 1

[server.auth]
methods = ["github"]

[server.web]
enabled = false

[server.auth.github]
allowed_usernames = ["alice"]

[server.integrations.github]
client_id = "Iv1.test"
"#,
        );
        let err = resolve_auth_mode_with_lookup(&file, |name| match name {
            "SESSION_SECRET" => {
                Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string())
            }
            "GITHUB_APP_CLIENT_SECRET" => Some("test-secret".to_string()),
            _ => None,
        })
        .expect_err("github auth should require web mode");
        assert!(err.to_string().contains("server.web.enabled"));
    }

    #[test]
    fn fails_when_github_session_secret_too_short() {
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
        let err = resolve_auth_mode_with_lookup(&file, |name| match name {
            "SESSION_SECRET" => Some("short-secret".to_string()),
            "GITHUB_APP_CLIENT_SECRET" => Some("test-secret".to_string()),
            _ => None,
        })
        .expect_err("short github session secret should fail");
        assert!(err.to_string().contains("at least 32 bytes"));
    }

    #[test]
    fn resolves_github_mode_with_jwt_key() {
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
        let mode = resolve_auth_mode_with_lookup(&file, |name| match name {
            "SESSION_SECRET" => {
                Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string())
            }
            "GITHUB_APP_CLIENT_SECRET" => Some("test-secret".to_string()),
            _ => None,
        })
        .expect("github auth should resolve");

        let AuthMode::Enabled(config) = mode else {
            panic!("expected enabled mode");
        };
        assert_eq!(config.methods, vec![ServerAuthMethod::Github]);
        assert!(config.jwt_key.is_some());
        assert_eq!(config.jwt_issuer.as_deref(), Some("http://localhost:3000"));
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
            methods:    vec![ServerAuthMethod::Github],
            dev_token:  None,
            jwt_key:    None,
            jwt_issuer: None,
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

    #[test]
    fn valid_jwt_bearer_authenticates_with_identity() {
        let token = issue_github_token(chrono::Duration::minutes(10));
        let parts = request_parts(
            github_jwt_mode(),
            Request::builder()
                .uri("/subject")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        );

        let auth = authenticate_parts(&parts).unwrap().unwrap();
        assert_eq!(auth.login, "octocat");
        assert_eq!(auth.auth_method, RunAuthMethod::Github);
        assert_eq!(auth.credential_source, CredentialSource::JwtAccessToken);
        assert_eq!(
            auth.identity,
            Some(IdpIdentity::new("https://github.com", "12345").unwrap())
        );
    }

    #[tokio::test]
    async fn expired_jwt_returns_machine_readable_code() {
        let token = issue_github_token(chrono::Duration::seconds(-10));
        let parts = request_parts(
            github_jwt_mode(),
            Request::builder()
                .uri("/subject")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        );

        let err = authenticate_parts(&parts).unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        let body = error_json(err).await;
        assert_eq!(body["errors"][0]["code"], "access_token_expired");
    }

    #[tokio::test]
    async fn jwt_with_bad_signature_returns_invalid_code() {
        let token = crate::auth::issue(
            &other_signing_key(),
            "https://fabro.example",
            &jwt_subject(),
            chrono::Duration::minutes(10),
        );
        let parts = request_parts(
            github_jwt_mode(),
            Request::builder()
                .uri("/subject")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        );

        let err = authenticate_parts(&parts).unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        let body = error_json(err).await;
        assert_eq!(body["errors"][0]["code"], "access_token_invalid");
    }

    #[tokio::test]
    async fn jwt_with_alg_none_returns_invalid_code() {
        let claims = serde_json::json!({
            "iss": "https://fabro.example",
            "aud": "fabro-cli",
            "sub": "12345",
            "exp": (chrono::Utc::now() + chrono::Duration::minutes(10)).timestamp(),
            "iat": chrono::Utc::now().timestamp(),
            "jti": uuid::Uuid::new_v4().to_string(),
            "idp_issuer": "https://github.com",
            "idp_subject": "12345",
            "login": "octocat",
            "name": "The Octocat",
            "email": "octocat@example.com",
            "auth_method": "github"
        });
        let token = format!(
            "{}.{}.signature",
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&serde_json::json!({ "alg": "none", "typ": "JWT" })).unwrap()
            ),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        );
        let parts = request_parts(
            github_jwt_mode(),
            Request::builder()
                .uri("/subject")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        );

        let err = authenticate_parts(&parts).unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        let body = error_json(err).await;
        assert_eq!(body["errors"][0]["code"], "access_token_invalid");
    }

    #[tokio::test]
    async fn malformed_jwt_like_bearer_returns_plain_unauthorized() {
        let parts = request_parts(
            github_jwt_mode(),
            Request::builder()
                .uri("/subject")
                .header("authorization", "Bearer eyJnot-a-jwt")
                .body(Body::empty())
                .unwrap(),
        );

        let err = authenticate_parts(&parts).unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        let body = error_json(err).await;
        assert_eq!(body["errors"][0]["detail"], "Authentication required.");
        assert_eq!(body["errors"][0].get("code"), None);
    }

    #[tokio::test]
    async fn refresh_token_bearer_logs_path_and_returns_unauthorized_code() {
        let parts = request_parts(
            github_jwt_mode(),
            Request::builder()
                .uri("/subject")
                .header("authorization", "Bearer fabro_refresh_test")
                .body(Body::empty())
                .unwrap(),
        );

        let (err, captured) = capture_logs(|| authenticate_parts(&parts).unwrap_err());
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        let body = error_json(err).await;
        assert_eq!(body["errors"][0]["code"], "unauthorized");

        let events = captured.lock().unwrap();
        assert!(events.iter().any(|event| {
            event.target == "fabro_server::jwt_auth"
                && event
                    .fields
                    .iter()
                    .any(|(field, value)| field == "path" && value.contains("/subject"))
        }));
    }
}
