use anyhow::{Result, anyhow};
#[cfg(test)]
use axum::http::request::Parts;
use axum::http::{HeaderMap, header};
use fabro_static::EnvVars;
use fabro_types::settings::{ServerAuthMethod, ServerNamespace};
use fabro_types::{AuthMethod, IdpIdentity};
use fabro_util::dev_token::validate_dev_token_format;
use hmac::{Hmac, Mac};
use sha2::Sha256;
#[cfg(test)]
use tracing::info;

#[cfg(test)]
use crate::auth::REFRESH_TOKEN_PREFIX;
use crate::auth::{self, JwtError, JwtSigningKey, KeyDeriveError};
use crate::error::ApiError;

type HmacSha256 = Hmac<Sha256>;
const DEV_TOKEN_COMPARE_KEY: &[u8] = b"fabro-dev-token-compare-key";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedAuth {
    pub login:       String,
    pub name:        String,
    pub email:       String,
    pub avatar_url:  String,
    pub user_url:    String,
    pub auth_method: AuthMethod,
    pub identity:    Option<IdpIdentity>,
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
}

pub fn resolve_auth_mode(settings: &ServerNamespace) -> Result<AuthMode> {
    resolve_auth_mode_with_lookup(settings, process_env_var)
}

#[expect(
    clippy::disallowed_methods,
    reason = "Server auth startup validation intentionally reads process env for server secrets."
)]
fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

pub fn resolve_auth_mode_with_lookup<F>(settings: &ServerNamespace, lookup: F) -> Result<AuthMode>
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
    if github_enabled && !web_enabled {
        return Err(anyhow!(
            "Fabro server refuses to start: github auth is enabled but server.web.enabled is false."
        ));
    }
    if github_enabled && settings.integrations.github.client_id.is_none() {
        return Err(anyhow!(
            "Fabro server refuses to start: github auth is enabled but server.integrations.github.client_id is not configured."
        ));
    }
    if github_enabled && lookup(EnvVars::GITHUB_APP_CLIENT_SECRET).is_none() {
        return Err(anyhow!(
            "Fabro server refuses to start: github auth is enabled but GITHUB_APP_CLIENT_SECRET is not set."
        ));
    }

    let session_secret = lookup(EnvVars::SESSION_SECRET);
    let secret = session_secret.as_deref().ok_or_else(|| {
        anyhow!("Fabro server refuses to start: auth is configured but SESSION_SECRET is not set.")
    })?;
    if web_enabled {
        auth::derive_cookie_key(secret.as_bytes()).map_err(|err| session_secret_key_error(&err))?;
    }

    let dev_token = if methods.contains(&ServerAuthMethod::DevToken) {
        let token = lookup(EnvVars::FABRO_DEV_TOKEN).ok_or_else(|| {
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

    let jwt_key = Some(
        auth::derive_jwt_key(secret.as_bytes()).map_err(|err| session_secret_key_error(&err))?,
    );
    let jwt_issuer = Some(resolve_jwt_issuer(settings, &lookup));

    Ok(AuthMode::Enabled(ConfiguredAuth {
        methods,
        dev_token,
        jwt_key,
        jwt_issuer,
    }))
}

fn resolve_jwt_issuer<F>(settings: &ServerNamespace, lookup: &F) -> String
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
        .or_else(|| {
            settings
                .api
                .url
                .as_ref()
                .and_then(|url| url.resolve(|name| lookup(name)).ok())
                .map(|resolved| resolved.value)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "fabro-server".to_string())
}

pub(crate) fn session_secret_key_error(err: &KeyDeriveError) -> anyhow::Error {
    match err {
        KeyDeriveError::Empty => {
            anyhow!(
                "Fabro server refuses to start: auth is configured but SESSION_SECRET is not set."
            )
        }
        KeyDeriveError::TooShort {
            got_bytes,
            min_bytes,
        } => anyhow!(
            "Fabro server refuses to start: SESSION_SECRET must be at least {min_bytes} bytes (64 hex characters) when auth is configured. Current length: {got_bytes} bytes."
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

fn config_allows_run_auth_method(config: &ConfiguredAuth, method: AuthMethod) -> bool {
    match method {
        AuthMethod::DevToken => config.methods.contains(&ServerAuthMethod::DevToken),
        AuthMethod::Github => config.methods.contains(&ServerAuthMethod::Github),
    }
}

pub(crate) fn bearer_token_from_headers(headers: &HeaderMap) -> Option<Result<&str, ApiError>> {
    let value = headers.get(header::AUTHORIZATION)?;
    let Ok(value) = value.to_str() else {
        return Some(Err(ApiError::unauthorized()));
    };
    Some(
        value
            .strip_prefix("Bearer ")
            .ok_or_else(ApiError::unauthorized),
    )
}

#[cfg(test)]
pub(crate) fn bearer_token(parts: &Parts) -> Option<Result<&str, ApiError>> {
    bearer_token_from_headers(&parts.headers)
}

pub(crate) fn authenticate_jwt_bearer(
    token: &str,
    config: &ConfiguredAuth,
) -> Result<VerifiedAuth, ApiError> {
    let Some(jwt_key) = config.jwt_key.as_ref() else {
        return Err(ApiError::unauthorized());
    };
    let Some(jwt_issuer) = config.jwt_issuer.as_deref() else {
        return Err(ApiError::unauthorized());
    };
    if !looks_like_jwt(token) {
        return Err(ApiError::unauthorized());
    }

    let claims = match auth::verify(jwt_key, jwt_issuer, token) {
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
        login:       claims.login,
        name:        claims.name,
        email:       claims.email,
        avatar_url:  claims.avatar_url,
        user_url:    claims.user_url,
        auth_method: claims.auth_method,
        identity:    Some(identity),
    })
}

#[cfg(test)]
fn authenticate_bearer(
    parts: &Parts,
    token: &str,
    config: &ConfiguredAuth,
) -> Result<VerifiedAuth, ApiError> {
    if token.starts_with(REFRESH_TOKEN_PREFIX) {
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

pub(crate) fn looks_like_jwt(token: &str) -> bool {
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

#[cfg(test)]
fn authenticate_parts(parts: &Parts) -> Result<Option<VerifiedAuth>, ApiError> {
    let auth_mode = parts
        .extensions
        .get::<AuthMode>()
        .expect("AuthMode extension must be added to the router");

    let AuthMode::Enabled(config) = auth_mode;

    authenticate_bearer(
        parts,
        bearer_token(parts).ok_or_else(ApiError::unauthorized)??,
        config,
    )
    .map(Some)
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
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use fabro_config::{Error as ConfigError, ServerSettingsBuilder};
    use fabro_types::IdpIdentity;
    use fabro_types::settings::ServerAuthMethod;
    use tracing::field::{Field, Visit};
    use tracing::{Event, Subscriber, subscriber};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::{Layer, Registry};

    use super::*;
    fn settings(source: &str) -> ServerNamespace {
        ServerSettingsBuilder::from_toml(source)
            .expect("fixture should resolve")
            .server
    }

    fn empty_lookup(_name: &str) -> Option<String> {
        None
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
            jwt_key:    Some(signing_key()),
            jwt_issuer: Some("https://fabro.example".to_string()),
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
        auth::derive_jwt_key(b"0123456789abcdef0123456789abcdef")
            .expect("jwt signing key should derive")
    }

    fn other_signing_key() -> JwtSigningKey {
        auth::derive_jwt_key(b"fedcba9876543210fedcba9876543210")
            .expect("jwt signing key should derive")
    }

    fn jwt_subject() -> auth::JwtSubject {
        auth::JwtSubject {
            identity:    IdpIdentity::new("https://github.com", "12345").unwrap(),
            login:       "octocat".to_string(),
            name:        "The Octocat".to_string(),
            email:       "octocat@example.com".to_string(),
            avatar_url:  "https://example.com/octocat.png".to_string(),
            user_url:    "https://github.com/octocat".to_string(),
            auth_method: AuthMethod::Github,
        }
    }

    fn issue_github_token(ttl: chrono::Duration) -> String {
        auth::issue(&signing_key(), "https://fabro.example", &jwt_subject(), ttl)
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
        let ConfigError::Resolve { errors, .. } = ServerSettingsBuilder::from_toml(
            r"
_version = 1

[server.auth]
methods = []
",
        )
        .expect_err("empty auth methods should fail") else {
            panic!("expected settings resolution error");
        };
        assert!(errors.iter().any(|err| matches!(
            err,
            fabro_config::ResolveError::Invalid { path, reason }
                if path == "server.auth.methods" && reason.contains("must not be empty")
        )));
    }

    #[test]
    fn fails_when_web_enabled_without_session_secret() {
        let file = settings(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]
"#,
        );
        let err = resolve_auth_mode_with_lookup(&file, empty_lookup)
            .expect_err("missing session secret should fail");
        assert!(err.to_string().contains("SESSION_SECRET"));
    }

    #[test]
    fn fails_when_dev_token_only_auth_lacks_session_secret() {
        let file = settings(
            r#"
_version = 1

[server.web]
enabled = false

[server.auth]
methods = ["dev-token"]
"#,
        );
        let err = resolve_auth_mode_with_lookup(&file, |name| match name {
            "FABRO_DEV_TOKEN" => Some(
                "fabro_dev_abababababababababababababababababababababababababababababababab"
                    .to_string(),
            ),
            _ => None,
        })
        .expect_err("dev-token auth should require session secret");
        assert!(err.to_string().contains("SESSION_SECRET"));
    }

    #[test]
    fn resolves_dev_token_mode_when_secrets_present() {
        let file = settings(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]
"#,
        );
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
        let AuthMode::Enabled(config) = mode;
        assert_eq!(config.methods, vec![ServerAuthMethod::DevToken]);
        assert!(config.dev_token.is_some());
        assert!(config.jwt_key.is_some());
        assert_eq!(config.jwt_issuer.as_deref(), Some("http://localhost:3000"));
    }

    #[test]
    fn uses_api_url_when_web_url_is_empty_for_jwt_issuer() {
        let file = settings(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.web]
url = ""

[server.api]
url = "http://localhost:4000"
"#,
        );
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
        let AuthMode::Enabled(config) = mode;
        assert_eq!(config.jwt_issuer.as_deref(), Some("http://localhost:4000"));
    }

    #[test]
    fn uses_literal_fallback_when_no_public_urls_are_configured() {
        let file = settings(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.web]
url = ""

[server.api]
url = ""
"#,
        );
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
        let AuthMode::Enabled(config) = mode;
        assert_eq!(config.jwt_issuer.as_deref(), Some("fabro-server"));
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

        let AuthMode::Enabled(config) = mode;
        assert_eq!(config.methods, vec![ServerAuthMethod::Github]);
        assert!(config.jwt_key.is_some());
        assert_eq!(config.jwt_issuer.as_deref(), Some("http://localhost:3000"));
    }

    #[tokio::test]
    async fn rejects_missing_credentials() {
        let parts = request_parts(
            dev_token_mode(),
            Request::builder().uri("/test").body(Body::empty()).unwrap(),
        );
        let err = authenticate_parts(&parts).unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_dev_token_bearer_without_translation() {
        let parts = request_parts(
            dev_token_mode(),
            Request::builder()
                .uri("/subject")
                .header(
                    "authorization",
                    "Bearer fabro_dev_abababababababababababababababababababababababababababababab",
                )
                .body(Body::empty())
                .unwrap(),
        );
        let err = authenticate_parts(&parts).unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn subject_reports_profile_fields_from_jwt() {
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
        assert_eq!(auth.name, "The Octocat");
        assert_eq!(auth.email, "octocat@example.com");
        assert_eq!(auth.avatar_url, "https://example.com/octocat.png");
        assert_eq!(auth.user_url, "https://github.com/octocat");
        assert_eq!(
            auth.identity,
            Some(IdpIdentity::new("https://github.com", "12345").unwrap())
        );
        assert_eq!(auth.auth_method, AuthMethod::Github);
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
        assert_eq!(auth.auth_method, AuthMethod::Github);
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
        let token = auth::issue(
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
