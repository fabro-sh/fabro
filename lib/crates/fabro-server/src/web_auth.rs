use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use cookie::time::Duration;
use cookie::{Cookie, CookieJar, Key, SameSite};
use fabro_types::RunAuthMethod;
use fabro_types::settings::{InterpString, ServerAuthMethod};
use fabro_util::dev_token::validate_dev_token_format;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{debug, error, info, warn};

use crate::jwt_auth::{AuthMode, auth_method_name, dev_token_matches};
use crate::server::AppState;

pub const SESSION_COOKIE_NAME: &str = "__fabro_session";
const OAUTH_STATE_COOKIE_NAME: &str = "fabro_oauth_state";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionCookie {
    pub v:           u8,
    pub login:       String,
    pub auth_method: RunAuthMethod,
    pub provider_id: Option<i64>,
    pub name:        String,
    pub email:       String,
    pub avatar_url:  String,
    pub user_url:    String,
    pub iat:         i64,
    pub exp:         i64,
}

#[derive(Deserialize)]
struct OAuthCallbackParams {
    code:  String,
    state: String,
}

#[derive(Deserialize)]
struct DevTokenLoginRequest {
    token: String,
}

#[derive(Deserialize)]
struct DemoToggleRequest {
    enabled: bool,
}

#[derive(Serialize)]
struct AuthConfigResponse {
    methods: Vec<String>,
}

#[derive(Serialize)]
struct AuthMeResponse {
    user:      SessionUser,
    provider:  String,
    #[serde(rename = "demoMode")]
    demo_mode: bool,
}

#[derive(Serialize)]
struct SessionUser {
    login:      String,
    name:       String,
    email:      String,
    #[serde(rename = "avatarUrl")]
    avatar_url: String,
    #[serde(rename = "userUrl")]
    user_url:   String,
}

#[derive(Deserialize)]
struct GitHubTokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct GitHubUser {
    id:         i64,
    login:      String,
    name:       Option<String>,
    avatar_url: String,
}

#[derive(Deserialize)]
struct GitHubEmail {
    email:    String,
    primary:  bool,
    verified: bool,
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/login/dev-token", post(login_dev_token))
        .route("/login/github", get(login_github))
        .route("/callback/github", get(callback_github))
        .route("/logout", post(logout))
}

pub fn api_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/auth/config", get(auth_config))
        .route("/auth/me", get(auth_me))
        .route("/demo/toggle", post(toggle_demo))
}

pub fn parse_cookie_header(headers: &HeaderMap) -> CookieJar {
    let mut jar = CookieJar::new();
    if let Some(raw) = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
    {
        for part in raw.split(';') {
            if let Ok(cookie) = Cookie::parse_encoded(part.trim().to_string()) {
                jar.add_original(cookie.into_owned());
            }
        }
    }
    jar
}

pub fn read_private_session(headers: &HeaderMap, key: &Key) -> Option<SessionCookie> {
    let jar = parse_cookie_header(headers);
    let cookie = jar.private(key).get(SESSION_COOKIE_NAME)?;
    let session: SessionCookie = serde_json::from_str(cookie.value()).ok()?;
    if session.v != 1 || session.exp <= chrono::Utc::now().timestamp() {
        return None;
    }
    Some(session)
}

fn read_private_oauth_state(headers: &HeaderMap, key: &Key) -> Option<String> {
    let jar = parse_cookie_header(headers);
    jar.private(key)
        .get(OAUTH_STATE_COOKIE_NAME)
        .map(|cookie| cookie.value().to_string())
}

fn add_oauth_state_cookie(jar: &mut CookieJar, key: &Key, state_token: String, secure: bool) {
    jar.private_mut(key).add(
        Cookie::build((OAUTH_STATE_COOKIE_NAME, state_token))
            .path("/auth")
            .http_only(true)
            .same_site(SameSite::Lax)
            .secure(secure)
            .max_age(Duration::minutes(10))
            .build(),
    );
}

fn remove_oauth_state_cookie(jar: &mut CookieJar, key: &Key, secure: bool) {
    jar.private_mut(key).remove(
        Cookie::build((OAUTH_STATE_COOKIE_NAME, ""))
            .path("/auth")
            .http_only(true)
            .secure(secure)
            .build(),
    );
}

fn append_jar_delta(headers: &mut HeaderMap, jar: &CookieJar) {
    for cookie in jar.delta() {
        if let Ok(value) = HeaderValue::from_str(&cookie.encoded().to_string()) {
            headers.append(header::SET_COOKIE, value);
        }
    }
}

fn json_response(status: StatusCode, body: serde_json::Value) -> Response {
    (status, Json(body)).into_response()
}

fn resolve_interp(value: &InterpString) -> anyhow::Result<String> {
    value
        .resolve(|name| std::env::var(name).ok())
        .map(|resolved| resolved.value)
        .map_err(anyhow::Error::from)
}

fn auth_methods_from_mode(auth_mode: &AuthMode) -> Vec<String> {
    match auth_mode {
        AuthMode::Enabled(config) => config
            .methods
            .iter()
            .map(|method| auth_method_name(*method).to_string())
            .collect(),
        AuthMode::Disabled => Vec::new(),
    }
}

fn auth_method_enabled(auth_mode: &AuthMode, method: ServerAuthMethod) -> bool {
    matches!(auth_mode, AuthMode::Enabled(config) if config.methods.contains(&method))
}

fn dev_token_from_mode(auth_mode: &AuthMode) -> Option<String> {
    match auth_mode {
        AuthMode::Enabled(config) => config.dev_token.clone(),
        AuthMode::Disabled => None,
    }
}

fn session_provider(auth_method: RunAuthMethod) -> &'static str {
    match auth_method {
        RunAuthMethod::Disabled => "disabled",
        RunAuthMethod::DevToken => "dev-token",
        RunAuthMethod::Github => "github",
    }
}

fn session_cookie_secure(state: &AppState) -> bool {
    state
        .server_settings()
        .web
        .url
        .resolve(|name| std::env::var(name).ok())
        .is_ok_and(|resolved| resolved.value.starts_with("https://"))
}

async fn login_dev_token(
    State(state): State<Arc<AppState>>,
    Extension(auth_mode): Extension<AuthMode>,
    Json(payload): Json<DevTokenLoginRequest>,
) -> Response {
    let expected = dev_token_from_mode(&auth_mode);
    let Some(expected) = expected else {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "Unauthorized"}));
    };

    if !validate_dev_token_format(&payload.token) || !dev_token_matches(&payload.token, &expected) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "Unauthorized"}));
    }

    let Some(session_key) = state.session_key() else {
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "SESSION_SECRET is not configured"}),
        );
    };

    let now = chrono::Utc::now();
    let session = SessionCookie {
        v:           1,
        login:       "dev".to_string(),
        auth_method: RunAuthMethod::DevToken,
        provider_id: None,
        name:        "Development User".to_string(),
        email:       "dev@localhost".to_string(),
        avatar_url:  "/logo.svg".to_string(),
        user_url:    String::new(),
        iat:         now.timestamp(),
        exp:         (now + chrono::Duration::days(30)).timestamp(),
    };

    let mut jar = CookieJar::new();
    jar.private_mut(&session_key).add(
        Cookie::build((
            SESSION_COOKIE_NAME,
            serde_json::to_string(&session).unwrap_or_default(),
        ))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(session_cookie_secure(state.as_ref()))
        .max_age(Duration::days(30))
        .build(),
    );

    let mut response = Json(json!({ "ok": true })).into_response();
    append_jar_delta(response.headers_mut(), &jar);
    response
}

async fn auth_config(Extension(auth_mode): Extension<AuthMode>) -> Response {
    Json(AuthConfigResponse {
        methods: auth_methods_from_mode(&auth_mode),
    })
    .into_response()
}

async fn login_github(
    State(state): State<Arc<AppState>>,
    Extension(auth_mode): Extension<AuthMode>,
) -> Response {
    if !auth_method_enabled(&auth_mode, ServerAuthMethod::Github) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "Unauthorized"}));
    }
    let Some(session_key) = state.session_key() else {
        warn!("OAuth login failed: SESSION_SECRET not configured");
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "SESSION_SECRET is not configured"}),
        );
    };
    let settings = state.server_settings();
    let Some(client_id) = settings.integrations.github.client_id.as_ref() else {
        warn!("OAuth login failed: client_id not configured");
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "GitHub App client_id is not configured"}),
        );
    };
    let client_id = match resolve_interp(client_id) {
        Ok(client_id) => client_id,
        Err(err) => {
            warn!(error = %err, "OAuth login failed: client_id could not be resolved");
            return json_response(
                StatusCode::CONFLICT,
                json!({"error": format!("GitHub App client_id could not be resolved: {err}")}),
            );
        }
    };
    let web_url = match resolve_interp(&settings.web.url) {
        Ok(web_url) => web_url,
        Err(err) => {
            warn!(error = %err, "OAuth login failed: server.web.url could not be resolved");
            return json_response(
                StatusCode::CONFLICT,
                json!({"error": format!("server.web.url could not be resolved: {err}")}),
            );
        }
    };
    if web_url.is_empty() {
        warn!("OAuth login failed: server.web.url not configured");
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "server.web.url is not configured"}),
        );
    }

    let state_token = format!("fabro-{}", ulid::Ulid::new());
    let authorize_url =
        fabro_http::Url::parse_with_params("https://github.com/login/oauth/authorize", &[
            ("client_id", client_id.as_str()),
            ("redirect_uri", &format!("{web_url}/auth/callback/github")),
            ("scope", "read:user user:email"),
            ("state", state_token.as_str()),
        ])
        .expect("GitHub authorize URL should be valid");

    debug!(redirect_uri = %format!("{web_url}/auth/callback/github"), "OAuth login redirecting to GitHub");

    let mut jar = CookieJar::new();
    add_oauth_state_cookie(
        &mut jar,
        &session_key,
        state_token,
        session_cookie_secure(state.as_ref()),
    );
    let mut response = Redirect::to(authorize_url.as_str()).into_response();
    append_jar_delta(response.headers_mut(), &jar);
    response
}

async fn callback_github(
    State(state): State<Arc<AppState>>,
    Extension(auth_mode): Extension<AuthMode>,
    Query(params): Query<OAuthCallbackParams>,
    headers: HeaderMap,
) -> Response {
    if !auth_method_enabled(&auth_mode, ServerAuthMethod::Github) {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "Unauthorized"}));
    }
    let Some(session_key) = state.session_key() else {
        error!("OAuth callback failed: SESSION_SECRET not configured");
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "SESSION_SECRET is not configured"}),
        );
    };
    let settings = state.server_settings();
    let stored_state = read_private_oauth_state(&headers, &session_key);
    if stored_state.as_deref() != Some(params.state.as_str()) {
        warn!("OAuth callback failed: state mismatch");
        return Redirect::to("/login").into_response();
    }

    let Some(client_id) = settings.integrations.github.client_id.as_ref() else {
        error!("OAuth callback failed: client_id not configured");
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "GitHub App client_id is not configured"}),
        );
    };
    let client_id = match resolve_interp(client_id) {
        Ok(client_id) => client_id,
        Err(err) => {
            error!(error = %err, "OAuth callback failed: client_id could not be resolved");
            return json_response(
                StatusCode::CONFLICT,
                json!({"error": format!("GitHub App client_id could not be resolved: {err}")}),
            );
        }
    };
    let Some(client_secret) = state.server_secret("GITHUB_APP_CLIENT_SECRET") else {
        error!("OAuth callback failed: GITHUB_APP_CLIENT_SECRET not configured");
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "GITHUB_APP_CLIENT_SECRET is not configured"}),
        );
    };
    let web_url = match resolve_interp(&settings.web.url) {
        Ok(web_url) => web_url,
        Err(err) => {
            error!(error = %err, "OAuth callback failed: server.web.url could not be resolved");
            return json_response(
                StatusCode::CONFLICT,
                json!({"error": format!("server.web.url could not be resolved: {err}")}),
            );
        }
    };

    let http = match fabro_http::http_client() {
        Ok(http) => http,
        Err(err) => {
            error!(error = %err, "OAuth callback failed: could not build GitHub HTTP client");
            return json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error": format!("Failed to build GitHub HTTP client: {err}")}),
            );
        }
    };
    let token = match http
        .post("https://github.com/login/oauth/access_token")
        .header(header::ACCEPT, "application/json")
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", params.code.as_str()),
            (
                "redirect_uri",
                format!("{web_url}/auth/callback/github").as_str(),
            ),
            ("state", params.state.as_str()),
        ])
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            match response.json::<GitHubTokenResponse>().await {
                Ok(token) => token.access_token,
                Err(err) => {
                    error!(error = %err, "OAuth callback failed: could not parse GitHub token response");
                    return json_response(
                        StatusCode::BAD_GATEWAY,
                        json!({"error": "Failed to parse GitHub token response"}),
                    );
                }
            }
        }
        Ok(response) => {
            let status = response.status();
            error!(status = %status, "OAuth callback failed: GitHub token exchange returned error");
            return json_response(
                StatusCode::BAD_GATEWAY,
                json!({"error": format!("GitHub token exchange failed: {status}")}),
            );
        }
        Err(err) => {
            error!(error = %err, "OAuth callback failed: GitHub token exchange request failed");
            return json_response(
                StatusCode::BAD_GATEWAY,
                json!({"error": format!("GitHub token exchange failed: {err}")}),
            );
        }
    };

    let auth_header = format!("Bearer {token}");
    let profile = match http
        .get("https://api.github.com/user")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::USER_AGENT, "fabro-server")
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => match response.json::<GitHubUser>().await
        {
            Ok(profile) => profile,
            Err(err) => {
                error!(error = %err, "OAuth callback failed: could not parse GitHub user response");
                return json_response(
                    StatusCode::BAD_GATEWAY,
                    json!({"error": "Failed to parse GitHub user response"}),
                );
            }
        },
        Ok(response) => {
            let status = response.status();
            error!(status = %status, "OAuth callback failed: GitHub user lookup returned error");
            return json_response(
                StatusCode::BAD_GATEWAY,
                json!({"error": format!("GitHub user lookup failed: {status}")}),
            );
        }
        Err(err) => {
            error!(error = %err, "OAuth callback failed: GitHub user lookup request failed");
            return json_response(
                StatusCode::BAD_GATEWAY,
                json!({"error": format!("GitHub user lookup failed: {err}")}),
            );
        }
    };

    let emails = match http
        .get("https://api.github.com/user/emails")
        .header(header::AUTHORIZATION, &auth_header)
        .header(header::USER_AGENT, "fabro-server")
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response
            .json::<Vec<GitHubEmail>>()
            .await
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    let allowed_usernames = settings.auth.github.allowed_usernames.clone();
    if !allowed_usernames.iter().any(|user| user == &profile.login) {
        warn!(login = %profile.login, "OAuth callback denied: username not in allowlist");
        return Redirect::to("/login?error=unauthorized").into_response();
    }

    let primary_email = emails
        .iter()
        .find(|email| email.primary && email.verified)
        .map(|email| email.email.clone())
        .unwrap_or_default();
    let now = chrono::Utc::now();
    let session = SessionCookie {
        v:           1,
        login:       profile.login.clone(),
        auth_method: RunAuthMethod::Github,
        provider_id: Some(profile.id),
        name:        profile.name.unwrap_or_else(|| profile.login.clone()),
        email:       primary_email,
        avatar_url:  profile.avatar_url,
        user_url:    format!("https://github.com/{}", profile.login),
        iat:         now.timestamp(),
        exp:         (now + chrono::Duration::days(30)).timestamp(),
    };

    info!(login = %session.login, "OAuth login succeeded");

    let mut jar = CookieJar::new();
    jar.private_mut(&session_key).add(
        Cookie::build((
            SESSION_COOKIE_NAME,
            serde_json::to_string(&session).unwrap_or_default(),
        ))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(session_cookie_secure(state.as_ref()))
        .max_age(Duration::days(30))
        .build(),
    );
    remove_oauth_state_cookie(
        &mut jar,
        &session_key,
        session_cookie_secure(state.as_ref()),
    );
    let mut response = Redirect::to("/runs").into_response();
    append_jar_delta(response.headers_mut(), &jar);
    response
}

async fn logout(State(state): State<Arc<AppState>>) -> Response {
    info!("User logged out");
    let mut jar = CookieJar::new();
    if let Some(key) = state.session_key() {
        jar.private_mut(&key).remove(
            Cookie::build((SESSION_COOKIE_NAME, ""))
                .path("/")
                .http_only(true)
                .secure(session_cookie_secure(state.as_ref()))
                .build(),
        );
    }
    let mut response = Redirect::to("/login").into_response();
    append_jar_delta(response.headers_mut(), &jar);
    response
}

async fn auth_me(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let has_cookie = headers.get(header::COOKIE).is_some();
    let Some(session_key) = state.session_key() else {
        warn!(
            has_cookie,
            "Auth check failed: SESSION_SECRET not available"
        );
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "Unauthorized"}));
    };
    let Some(session) = read_private_session(&headers, &session_key) else {
        warn!(
            has_cookie,
            "Auth check failed: session cookie missing or decryption failed"
        );
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "Unauthorized"}));
    };

    let demo_mode = parse_cookie_header(&headers)
        .get("fabro-demo")
        .is_some_and(|cookie| cookie.value() == "1");
    Json(AuthMeResponse {
        user: SessionUser {
            login:      session.login,
            name:       session.name,
            email:      session.email,
            avatar_url: session.avatar_url,
            user_url:   session.user_url,
        },
        provider: session_provider(session.auth_method).to_string(),
        demo_mode,
    })
    .into_response()
}

async fn toggle_demo(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<DemoToggleRequest>,
) -> Response {
    let mut jar = CookieJar::new();
    jar.add(
        Cookie::build(("fabro-demo", if payload.enabled { "1" } else { "0" }))
            .path("/")
            .http_only(true)
            .same_site(SameSite::Lax)
            .secure(session_cookie_secure(state.as_ref()))
            .max_age(Duration::days(365))
            .build(),
    );
    let mut response = Json(json!({ "enabled": payload.enabled })).into_response();
    append_jar_delta(response.headers_mut(), &jar);
    response
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Extension;
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{Request, StatusCode, header};
    use axum_extra::extract::cookie::Key;
    use fabro_test::{assert_axum_status, expect_axum_json};
    use fabro_types::RunAuthMethod;
    use fabro_types::settings::SettingsLayer;
    use fabro_types::settings::server::{
        GithubIntegrationLayer, ServerAuthGithubLayer, ServerAuthLayer, ServerAuthMethod,
        ServerIntegrationsLayer, ServerLayer, ServerWebLayer,
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::{api_routes, read_private_session, routes};
    use crate::jwt_auth::{AuthMode, ConfiguredAuth};
    use crate::server;

    const DEV_TOKEN: &str =
        "fabro_dev_abababababababababababababababababababababababababababababababab";

    fn dev_token_auth_mode() -> AuthMode {
        AuthMode::Enabled(ConfiguredAuth {
            methods:   vec![ServerAuthMethod::DevToken],
            dev_token: Some(DEV_TOKEN.to_string()),
        })
    }

    fn github_auth_mode() -> AuthMode {
        AuthMode::Enabled(ConfiguredAuth {
            methods:   vec![ServerAuthMethod::Github],
            dev_token: None,
        })
    }

    fn github_settings(web_url: &str) -> SettingsLayer {
        SettingsLayer {
            server: Some(ServerLayer {
                web: Some(ServerWebLayer {
                    enabled: Some(true),
                    url:     Some(web_url.into()),
                }),
                auth: Some(ServerAuthLayer {
                    methods: Some(vec![ServerAuthMethod::Github]),
                    github:  Some(ServerAuthGithubLayer {
                        allowed_usernames: vec!["octocat".to_string()],
                    }),
                }),
                integrations: Some(ServerIntegrationsLayer {
                    github: Some(GithubIntegrationLayer {
                        client_id: Some("github-client-id".into()),
                        ..GithubIntegrationLayer::default()
                    }),
                    ..ServerIntegrationsLayer::default()
                }),
                ..ServerLayer::default()
            }),
            ..SettingsLayer::default()
        }
    }

    fn test_auth_router_with_settings(
        settings: SettingsLayer,
        auth_mode: AuthMode,
    ) -> axum::Router {
        let state = server::create_test_app_state_with_session_key(
            settings,
            Some("web-auth-test-key-material-0123456789"),
            false,
        );
        let middleware_state = state.clone();
        axum::Router::new()
            .nest("/auth", routes())
            .nest("/api/v1", api_routes())
            .layer(Extension(auth_mode))
            .layer(axum::middleware::from_fn_with_state(
                middleware_state,
                |State(state): State<Arc<crate::server::AppState>>,
                 mut req: axum::extract::Request,
                 next: axum::middleware::Next| async move {
                    if let Some(key) = state.session_key() {
                        if let Some(session) = read_private_session(req.headers(), &key) {
                            req.extensions_mut().insert(session);
                        }
                    }
                    next.run(req).await
                },
            ))
            .with_state(state)
    }

    fn test_auth_router(_key: &Key, auth_mode: AuthMode) -> axum::Router {
        test_auth_router_with_settings(SettingsLayer::default(), auth_mode)
    }

    async fn response_json(response: axum::response::Response) -> Value {
        expect_axum_json(response, StatusCode::OK, "response_json").await
    }

    async fn assert_status(response: axum::response::Response, expected: StatusCode) {
        assert_axum_status(response, expected, "assert_status").await;
    }

    async fn checked_response(
        response: axum::response::Response,
        expected: StatusCode,
    ) -> axum::response::Response {
        fabro_test::expect_axum_status(response, expected, "checked_response").await
    }

    #[tokio::test]
    async fn login_dev_token_mints_session_with_dev_token_provider() {
        let key = Key::derive_from(b"web-auth-test-key-material-0123456789");
        let app = test_auth_router(&key, dev_token_auth_mode());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/login/dev-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "token": DEV_TOKEN }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let response = checked_response(response, StatusCode::OK).await;

        let session_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .expect("session cookie should be set")
            .to_string();

        let mut cookie_headers = axum::http::HeaderMap::new();
        cookie_headers.insert(
            header::COOKIE,
            axum::http::HeaderValue::from_str(&session_cookie).unwrap(),
        );
        let session = read_private_session(&cookie_headers, &key).expect("session should decode");
        assert_eq!(session.auth_method, RunAuthMethod::DevToken);
        assert_eq!(session.v, 1);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/me")
                    .header(header::COOKIE, &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response_json(response).await;
        assert_eq!(body["provider"], "dev-token");
        assert_eq!(body["user"]["login"], "dev");
    }

    #[tokio::test]
    async fn login_dev_token_rejects_invalid_token() {
        let key = Key::derive_from(b"web-auth-test-key-material-0123456789");
        let app = test_auth_router(&key, dev_token_auth_mode());

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/login/dev-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "token": "fabro_dev_cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd" })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_status(response, StatusCode::UNAUTHORIZED).await;
    }

    #[tokio::test]
    async fn auth_config_returns_dev_token_method() {
        let key = Key::derive_from(b"web-auth-test-key-material-0123456789");
        let app = test_auth_router(&key, dev_token_auth_mode());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response_json(response).await;
        assert_eq!(body, json!({ "methods": ["dev-token"] }));
    }

    #[tokio::test]
    async fn login_github_sets_secure_state_cookie_for_https_web_url() {
        let app = test_auth_router_with_settings(
            github_settings("https://fabro.example"),
            github_auth_mode(),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/auth/login/github")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let response = checked_response(response, StatusCode::SEE_OTHER).await;
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .expect("oauth state cookie should be set");
        assert!(
            set_cookie.contains("Secure"),
            "state cookie should be marked Secure: {set_cookie}"
        );
    }

    #[tokio::test]
    async fn callback_github_rejects_plain_oauth_state_cookie() {
        let app = test_auth_router_with_settings(
            github_settings("https://fabro.example"),
            github_auth_mode(),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/auth/callback/github?code=test-code&state=fabro-test-state")
                    .header(header::COOKIE, "fabro_oauth_state=fabro-test-state")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let response = checked_response(response, StatusCode::SEE_OTHER).await;
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok()),
            Some("/login")
        );
    }

    #[test]
    fn read_private_session_rejects_cookies_without_version() {
        let key = Key::derive_from(b"web-auth-test-key-material-0123456789");
        let mut jar = cookie::CookieJar::new();
        jar.private_mut(&key).add(cookie::Cookie::new(
            super::SESSION_COOKIE_NAME,
            json!({
                "login": "dev",
                "provider": "dev-token",
                "name": "Development User",
                "email": "dev@localhost",
                "avatar_url": "/logo.svg",
                "user_url": "",
                "provider_id": null,
                "exp": chrono::Utc::now().timestamp() + 60,
            })
            .to_string(),
        ));
        let encoded = jar
            .delta()
            .next()
            .expect("private cookie should exist")
            .encoded()
            .to_string();

        let mut headers = axum::http::HeaderMap::new();
        headers.insert(header::COOKIE, encoded.parse().unwrap());
        assert!(read_private_session(&headers, &key).is_none());
    }
}
