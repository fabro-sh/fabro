use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use cookie::time::Duration;
use cookie::{Cookie, CookieJar, Key, SameSite};
use fabro_types::settings::ServerAuthMethod;
use fabro_types::{IdpIdentity, RunAuthMethod};
use fabro_util::dev_token::validate_dev_token_format;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{debug, error, info, warn};

use crate::auth::GithubEndpoints;
use crate::jwt_auth::{
    AuthMode, AuthenticatedService, AuthenticatedSubject, auth_method_name, dev_token_matches,
};
use crate::server::AppState;

pub const SESSION_COOKIE_NAME: &str = "__fabro_session";
const OAUTH_STATE_COOKIE_NAME: &str = "fabro_oauth_state";
const OAUTH_STATE_TTL_MINUTES: i64 = 30;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionCookie {
    pub v:           u8,
    pub login:       String,
    pub auth_method: RunAuthMethod,
    pub identity:    Option<IdpIdentity>,
    pub name:        String,
    pub email:       String,
    pub avatar_url:  String,
    pub user_url:    String,
    pub iat:         i64,
    pub exp:         i64,
}

#[derive(Deserialize)]
struct OAuthCallbackParams {
    code:  Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct LoginGithubParams {
    return_to: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OAuthStateCookie {
    state:     String,
    exp:       i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    return_to: Option<String>,
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
    login:       String,
    name:        String,
    email:       String,
    #[serde(rename = "idpIssuer", skip_serializing_if = "Option::is_none")]
    idp_issuer:  Option<String>,
    #[serde(rename = "idpSubject", skip_serializing_if = "Option::is_none")]
    idp_subject: Option<String>,
    #[serde(rename = "avatarUrl")]
    avatar_url:  String,
    #[serde(rename = "userUrl")]
    user_url:    String,
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
    if session.v != 2 || session.exp <= chrono::Utc::now().timestamp() {
        return None;
    }
    Some(session)
}

fn read_private_oauth_state(headers: &HeaderMap, key: &Key) -> Option<OAuthStateCookie> {
    let jar = parse_cookie_header(headers);
    jar.private(key)
        .get(OAUTH_STATE_COOKIE_NAME)
        .and_then(|cookie| serde_json::from_str(cookie.value()).ok())
        .filter(|state: &OAuthStateCookie| state.exp > chrono::Utc::now().timestamp())
}

fn add_oauth_state_cookie(jar: &mut CookieJar, key: &Key, state: &OAuthStateCookie, secure: bool) {
    jar.private_mut(key).add(
        Cookie::build((
            OAUTH_STATE_COOKIE_NAME,
            serde_json::to_string(&state).unwrap_or_default(),
        ))
        .path("/auth")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(secure)
        .max_age(Duration::minutes(OAUTH_STATE_TTL_MINUTES))
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

fn static_error_page(body: &'static str) -> Response {
    let mut response = (
        StatusCode::BAD_REQUEST,
        Html(format!(
            "<!doctype html><html><body><p>{body}</p></body></html>"
        )),
    )
        .into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn sanitize_return_to(return_to: Option<String>) -> Option<String> {
    match return_to {
        Some(path) if matches!(path.as_str(), "/auth/cli/start" | "/auth/cli/resume") => Some(path),
        Some(_) => {
            warn!("Ignoring unsupported OAuth return_to path");
            None
        }
        None => None,
    }
}

fn oauth_error_redirect(path: &str, state: &str, error: &str, error_description: &str) -> String {
    const QUERY_VALUE_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC.remove(b'_').remove(b'-');
    let error = utf8_percent_encode(error, QUERY_VALUE_ENCODE_SET);
    let error_description = utf8_percent_encode(error_description, QUERY_VALUE_ENCODE_SET);
    let state = utf8_percent_encode(state, QUERY_VALUE_ENCODE_SET);
    format!("{path}?error={error}&error_description={error_description}&state={state}")
}

fn callback_error_redirect(
    return_to: Option<&str>,
    fallback: &'static str,
    state: &str,
    error: &'static str,
    error_description: &'static str,
) -> Response {
    match return_to {
        Some(path) => Redirect::to(&oauth_error_redirect(path, state, error, error_description))
            .into_response(),
        None => Redirect::to(fallback).into_response(),
    }
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
        .server
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
        v:           2,
        login:       "dev".to_string(),
        auth_method: RunAuthMethod::DevToken,
        identity:    Some(IdpIdentity::new("fabro:dev", "dev").expect("non-empty dev identity")),
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
    Extension(github_endpoints): Extension<Arc<GithubEndpoints>>,
    Query(params): Query<LoginGithubParams>,
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
    let Some(client_id) = settings.server.integrations.github.client_id.as_ref() else {
        warn!("OAuth login failed: client_id not configured");
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "GitHub App client_id is not configured"}),
        );
    };
    let client_id = match state.resolve_interp(client_id) {
        Ok(client_id) => client_id,
        Err(err) => {
            warn!(error = %err, "OAuth login failed: client_id could not be resolved");
            return json_response(
                StatusCode::CONFLICT,
                json!({"error": format!("GitHub App client_id could not be resolved: {err}")}),
            );
        }
    };
    let web_url = match state.canonical_origin() {
        Ok(web_url) => web_url,
        Err(err) => {
            warn!(error = %err, "OAuth login failed: server.web.url is invalid");
            return json_response(StatusCode::CONFLICT, json!({"error": err}));
        }
    };

    let state_token = format!("fabro-{}", ulid::Ulid::new());
    let authorize_url = fabro_http::Url::parse_with_params(
        github_endpoints
            .oauth_base
            .join("login/oauth/authorize")
            .expect("GitHub authorize URL should be valid")
            .as_str(),
        &[
            ("client_id", client_id.as_str()),
            ("redirect_uri", &format!("{web_url}/auth/callback/github")),
            ("scope", "read:user user:email"),
            ("state", state_token.as_str()),
        ],
    )
    .expect("GitHub authorize URL should be valid");

    debug!(redirect_uri = %format!("{web_url}/auth/callback/github"), "OAuth login redirecting to GitHub");

    let mut jar = CookieJar::new();
    add_oauth_state_cookie(
        &mut jar,
        &session_key,
        &OAuthStateCookie {
            state:     state_token,
            exp:       (chrono::Utc::now() + chrono::Duration::minutes(OAUTH_STATE_TTL_MINUTES))
                .timestamp(),
            return_to: sanitize_return_to(params.return_to),
        },
        session_cookie_secure(state.as_ref()),
    );
    let mut response = Redirect::to(authorize_url.as_str()).into_response();
    append_jar_delta(response.headers_mut(), &jar);
    response
}

async fn callback_github(
    State(state): State<Arc<AppState>>,
    Extension(auth_mode): Extension<AuthMode>,
    Extension(github_endpoints): Extension<Arc<GithubEndpoints>>,
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
    let Some(stored_state) = stored_state else {
        warn!("OAuth callback failed: state cookie missing or invalid");
        return static_error_page(
            "Your login took too long or was tampered with. Please start again.",
        );
    };
    if stored_state.state.as_str() != params.state.as_deref().unwrap_or_default() {
        warn!("OAuth callback failed: state mismatch");
        return static_error_page(
            "Your login took too long or was tampered with. Please start again.",
        );
    }

    if let Some(error_code) = params.error.as_deref() {
        let (error, error_description, fallback) = match error_code {
            "unauthorized" => (
                "unauthorized",
                "Login not permitted",
                "/login?error=unauthorized",
            ),
            "access_denied" => (
                "access_denied",
                "Authorization denied",
                "/login?error=access_denied",
            ),
            _ => (
                "server_error",
                "Could not complete GitHub sign-in",
                "/login?error=server_error",
            ),
        };
        let mut jar = CookieJar::new();
        remove_oauth_state_cookie(
            &mut jar,
            &session_key,
            session_cookie_secure(state.as_ref()),
        );
        let mut response = callback_error_redirect(
            stored_state.return_to.as_deref(),
            fallback,
            &stored_state.state,
            error,
            error_description,
        );
        append_jar_delta(response.headers_mut(), &jar);
        return response;
    }

    let Some(code) = params.code.as_deref() else {
        warn!("OAuth callback failed: code missing from successful callback");
        return callback_error_redirect(
            stored_state.return_to.as_deref(),
            "/login?error=server_error",
            &stored_state.state,
            "server_error",
            "Could not complete GitHub sign-in",
        );
    };
    let state_param = params
        .state
        .as_deref()
        .expect("validated oauth callback state should exist");

    let Some(client_id) = settings.server.integrations.github.client_id.as_ref() else {
        error!("OAuth callback failed: client_id not configured");
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "GitHub App client_id is not configured"}),
        );
    };
    let client_id = match state.resolve_interp(client_id) {
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
    let web_url = match state.canonical_origin() {
        Ok(web_url) => web_url,
        Err(err) => {
            error!(error = %err, "OAuth callback failed: server.web.url is invalid");
            return json_response(StatusCode::CONFLICT, json!({"error": err}));
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
        .post(
            github_endpoints
                .oauth_base
                .join("login/oauth/access_token")
                .expect("GitHub token URL should be valid"),
        )
        .header(header::ACCEPT, "application/json")
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", code),
            (
                "redirect_uri",
                format!("{web_url}/auth/callback/github").as_str(),
            ),
            ("state", state_param),
        ])
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            match response.json::<GitHubTokenResponse>().await {
                Ok(token) => token.access_token,
                Err(err) => {
                    error!(error = %err, "OAuth callback failed: could not parse GitHub token response");
                    return callback_error_redirect(
                        stored_state.return_to.as_deref(),
                        "/login?error=server_error",
                        &stored_state.state,
                        "server_error",
                        "Could not complete GitHub sign-in",
                    );
                }
            }
        }
        Ok(response) => {
            let status = response.status();
            error!(status = %status, "OAuth callback failed: GitHub token exchange returned error");
            return callback_error_redirect(
                stored_state.return_to.as_deref(),
                "/login?error=server_error",
                &stored_state.state,
                "server_error",
                "Could not complete GitHub sign-in",
            );
        }
        Err(err) => {
            error!(error = %err, "OAuth callback failed: GitHub token exchange request failed");
            return callback_error_redirect(
                stored_state.return_to.as_deref(),
                "/login?error=server_error",
                &stored_state.state,
                "server_error",
                "Could not complete GitHub sign-in",
            );
        }
    };

    let auth_header = format!("Bearer {token}");
    let profile = match http
        .get(
            github_endpoints
                .api_base
                .join("user")
                .expect("GitHub user URL should be valid"),
        )
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
                return callback_error_redirect(
                    stored_state.return_to.as_deref(),
                    "/login?error=server_error",
                    &stored_state.state,
                    "server_error",
                    "Could not complete GitHub sign-in",
                );
            }
        },
        Ok(response) => {
            let status = response.status();
            error!(status = %status, "OAuth callback failed: GitHub user lookup returned error");
            return callback_error_redirect(
                stored_state.return_to.as_deref(),
                "/login?error=server_error",
                &stored_state.state,
                "server_error",
                "Could not complete GitHub sign-in",
            );
        }
        Err(err) => {
            error!(error = %err, "OAuth callback failed: GitHub user lookup request failed");
            return callback_error_redirect(
                stored_state.return_to.as_deref(),
                "/login?error=server_error",
                &stored_state.state,
                "server_error",
                "Could not complete GitHub sign-in",
            );
        }
    };

    let emails = match http
        .get(
            github_endpoints
                .api_base
                .join("user/emails")
                .expect("GitHub emails URL should be valid"),
        )
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

    let allowed_usernames = settings.server.auth.github.allowed_usernames.clone();
    if !allowed_usernames.iter().any(|user| user == &profile.login) {
        warn!(login = %profile.login, "OAuth callback denied: username not in allowlist");
        return callback_error_redirect(
            stored_state.return_to.as_deref(),
            "/login?error=unauthorized",
            &stored_state.state,
            "unauthorized",
            "Login not permitted",
        );
    }

    let primary_email = emails
        .iter()
        .find(|email| email.primary && email.verified)
        .map(|email| email.email.clone())
        .unwrap_or_default();
    let now = chrono::Utc::now();
    let session = SessionCookie {
        v:           2,
        login:       profile.login.clone(),
        auth_method: RunAuthMethod::Github,
        identity:    Some(
            IdpIdentity::new("https://github.com", profile.id.to_string())
                .expect("GitHub profile id should produce a valid identity"),
        ),
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
    let redirect_target = stored_state
        .return_to
        .as_deref()
        .unwrap_or("/runs")
        .to_string();
    let mut response = Redirect::to(&redirect_target).into_response();
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

async fn auth_me(subject: AuthenticatedSubject, headers: HeaderMap) -> Response {
    if subject.login.is_none() {
        warn!(
            has_cookie = headers.get(header::COOKIE).is_some(),
            "Auth check failed: authenticated subject missing"
        );
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "Unauthorized"}));
    }

    let demo_mode = parse_cookie_header(&headers)
        .get("fabro-demo")
        .is_some_and(|cookie| cookie.value() == "1");
    Json(AuthMeResponse {
        user: SessionUser {
            login:       subject.login.expect("checked above"),
            name:        subject.name,
            email:       subject.email,
            idp_issuer:  subject
                .identity
                .as_ref()
                .map(|identity| identity.issuer().to_string()),
            idp_subject: subject
                .identity
                .as_ref()
                .map(|identity| identity.subject().to_string()),
            avatar_url:  subject.avatar_url,
            user_url:    subject.user_url,
        },
        provider: session_provider(subject.auth_method).to_string(),
        demo_mode,
    })
    .into_response()
}

async fn toggle_demo(
    _auth: AuthenticatedService,
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
    use axum::body::{Body, to_bytes};
    use axum::http::{HeaderMap, Request, StatusCode, header};
    use axum_extra::extract::cookie::Key;
    use fabro_config::{RunLayer, ServerSettingsBuilder};
    use fabro_types::settings::server::ServerAuthMethod;
    use fabro_types::{IdpIdentity, RunAuthMethod};
    use serde_json::json;
    use tower::ServiceExt;

    use super::{
        OAUTH_STATE_TTL_MINUTES, api_routes, read_private_oauth_state, read_private_session, routes,
    };
    use crate::auth::{self, GithubEndpoints};
    use crate::jwt_auth::{AuthMode, ConfiguredAuth};
    use crate::server;

    const DEV_TOKEN: &str =
        "fabro_dev_abababababababababababababababababababababababababababababababab";

    fn test_cookie_key() -> Key {
        auth::derive_cookie_key(b"web-auth-test-key-material-0123456789")
            .expect("test key should derive")
    }

    fn dev_token_auth_mode() -> AuthMode {
        AuthMode::Enabled(ConfiguredAuth {
            methods:    vec![ServerAuthMethod::DevToken],
            dev_token:  Some(DEV_TOKEN.to_string()),
            jwt_key:    Some(test_jwt_key()),
            jwt_issuer: Some("https://fabro.example".to_string()),
        })
    }

    fn github_auth_mode() -> AuthMode {
        AuthMode::Enabled(ConfiguredAuth {
            methods:    vec![ServerAuthMethod::Github],
            dev_token:  None,
            jwt_key:    Some(test_jwt_key()),
            jwt_issuer: Some("https://fabro.example".to_string()),
        })
    }

    fn test_jwt_key() -> auth::JwtSigningKey {
        auth::derive_jwt_key(b"web-auth-test-key-material-0123456789")
            .expect("test JWT key should derive")
    }

    fn default_settings() -> fabro_types::ServerSettings {
        ServerSettingsBuilder::from_toml(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]
"#,
        )
        .expect("default test settings should resolve")
    }

    fn github_settings(web_url: &str) -> fabro_types::ServerSettings {
        ServerSettingsBuilder::from_toml(&format!(
            r#"
_version = 1

[server.web]
enabled = true
url = "{web_url}"

[server.auth]
methods = ["github"]

[server.auth.github]
allowed_usernames = ["octocat"]

[server.integrations.github]
client_id = "github-client-id"
"#
        ))
        .expect("github settings should resolve")
    }

    fn test_auth_router_with_settings(
        settings: fabro_types::ServerSettings,
        auth_mode: AuthMode,
    ) -> axum::Router {
        let state = server::create_test_app_state_with_runtime_settings_and_session_key(
            settings,
            RunLayer::default(),
            Some("web-auth-test-key-material-0123456789"),
        );
        let middleware_state = state.clone();
        axum::Router::new()
            .nest("/auth", routes())
            .nest("/api/v1", api_routes())
            .layer(axum::middleware::from_fn_with_state(
                middleware_state,
                crate::auth::auth_translation_middleware,
            ))
            .layer(Extension(Arc::new(GithubEndpoints::production_defaults())))
            .layer(Extension(auth_mode))
            .with_state(state)
    }

    fn test_auth_router(_key: &Key, auth_mode: AuthMode) -> axum::Router {
        test_auth_router_with_settings(default_settings(), auth_mode)
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

    macro_rules! checked_response {
        ($response:expr, $expected:expr) => {
            fabro_test::expect_axum_status($response, $expected, concat!(file!(), ":", line!()))
        };
    }

    #[tokio::test]
    async fn login_dev_token_mints_session_with_dev_token_provider() {
        let key = test_cookie_key();
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
        let response = checked_response!(response, StatusCode::OK).await;

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
        assert_eq!(session.v, 2);
        assert_eq!(
            session.identity,
            Some(IdpIdentity::new("fabro:dev", "dev").unwrap())
        );

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
        let body = response_json!(response).await;
        assert_eq!(body["provider"], "dev-token");
        assert_eq!(body["user"]["login"], "dev");
        assert_eq!(body["user"]["idpIssuer"], "fabro:dev");
        assert_eq!(body["user"]["idpSubject"], "dev");
    }

    #[tokio::test]
    async fn auth_me_accepts_cli_jwt_with_empty_profile_urls() {
        let app = test_auth_router_with_settings(
            github_settings("https://fabro.example"),
            github_auth_mode(),
        );
        let token = auth::issue(
            &test_jwt_key(),
            "https://fabro.example",
            &auth::JwtSubject {
                identity:    IdpIdentity::new("https://github.com", "12345").unwrap(),
                login:       "octocat".to_string(),
                name:        "The Octocat".to_string(),
                email:       "octocat@example.com".to_string(),
                avatar_url:  String::new(),
                user_url:    String::new(),
                auth_method: RunAuthMethod::Github,
            },
            chrono::Duration::minutes(10),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/me")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = response_json!(response).await;
        assert_eq!(body["provider"], "github");
        assert_eq!(body["user"]["login"], "octocat");
        assert_eq!(body["user"]["avatarUrl"], "");
        assert_eq!(body["user"]["userUrl"], "");
    }

    #[tokio::test]
    async fn auth_me_returns_unauthorized_under_demo_mode_without_jwt() {
        let app = test_auth_router_with_settings(default_settings(), dev_token_auth_mode());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/me")
                    .header(header::COOKIE, "fabro-demo=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_status!(response, StatusCode::UNAUTHORIZED).await;
    }

    #[tokio::test]
    async fn login_dev_token_rejects_invalid_token() {
        let key = test_cookie_key();
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
        assert_status!(response, StatusCode::UNAUTHORIZED).await;
    }

    #[tokio::test]
    async fn auth_config_returns_dev_token_method() {
        let key = test_cookie_key();
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
        let body = response_json!(response).await;
        assert_eq!(body, json!({ "methods": ["dev-token"] }));
    }

    #[tokio::test]
    async fn auth_config_returns_real_methods_when_demo_cookie_set() {
        let state = server::create_test_app_state_with_runtime_settings_and_session_key(
            github_settings("https://fabro.example"),
            RunLayer::default(),
            Some("web-auth-test-key-material-0123456789"),
        );
        let app = server::build_router_with_options(
            state,
            &github_auth_mode(),
            Arc::new(crate::ip_allowlist::IpAllowlistConfig::default()),
            server::RouterOptions::default(),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/config")
                    .header(header::COOKIE, "fabro-demo=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response_json!(response).await;
        assert_eq!(body, json!({ "methods": ["github"] }));
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
        let response = checked_response!(response, StatusCode::SEE_OTHER).await;
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
    async fn login_github_persists_allowed_cli_return_to_in_state_cookie() {
        let key = test_cookie_key();
        let app = test_auth_router_with_settings(
            github_settings("https://fabro.example"),
            github_auth_mode(),
        );
        let now = chrono::Utc::now().timestamp();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/auth/login/github?return_to=/auth/cli/resume")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .expect("oauth state cookie should be set")
            .to_string();

        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, cookie.parse().unwrap());
        let oauth_state =
            read_private_oauth_state(&headers, &key).expect("oauth state should decode");
        assert_eq!(oauth_state.return_to.as_deref(), Some("/auth/cli/resume"));
        assert!(oauth_state.exp >= now + (29 * 60));
        assert!(oauth_state.exp <= now + (OAUTH_STATE_TTL_MINUTES * 60) + 5);
    }

    #[tokio::test]
    async fn login_github_uses_injected_github_endpoints() {
        let state = server::create_test_app_state_with_runtime_settings_and_session_key(
            github_settings("https://fabro.example"),
            RunLayer::default(),
            Some("web-auth-test-key-material-0123456789"),
        );
        let app = crate::server::build_router_with_options(
            state,
            &github_auth_mode(),
            Arc::new(crate::ip_allowlist::IpAllowlistConfig::default()),
            crate::server::RouterOptions {
                web_enabled:                 true,
                github_endpoints:            Some(Arc::new(GithubEndpoints::with_bases(
                    "http://127.0.0.1:12345/"
                        .parse()
                        .expect("oauth base should parse"),
                    "http://127.0.0.1:12345/api/"
                        .parse()
                        .expect("api base should parse"),
                ))),
                github_webhook_ip_allowlist: None,
            },
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

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .expect("redirect location should be set");
        assert!(location.starts_with("http://127.0.0.1:12345/login/oauth/authorize?"));
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

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            html.contains("Your login took too long or was tampered with. Please start again.")
        );
    }

    #[tokio::test]
    async fn callback_github_rejects_expired_oauth_state_cookie_after_35_minutes() {
        let key = test_cookie_key();
        let app = test_auth_router_with_settings(
            github_settings("https://fabro.example"),
            github_auth_mode(),
        );
        let mut jar = cookie::CookieJar::new();
        super::add_oauth_state_cookie(
            &mut jar,
            &key,
            &super::OAuthStateCookie {
                state:     "fabro-test-state".to_string(),
                exp:       (chrono::Utc::now() - chrono::Duration::minutes(5)).timestamp(),
                return_to: Some("/auth/cli/resume".to_string()),
            },
            true,
        );
        let cookie = jar
            .delta()
            .next()
            .expect("private oauth cookie should exist")
            .encoded()
            .to_string();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/auth/callback/github?code=test-code&state=fabro-test-state")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            html.contains("Your login took too long or was tampered with. Please start again.")
        );
    }

    #[tokio::test]
    async fn callback_github_forwards_sanitized_error_to_cli_return_to() {
        let key = test_cookie_key();
        let app = test_auth_router_with_settings(
            github_settings("https://fabro.example"),
            github_auth_mode(),
        );
        let mut jar = cookie::CookieJar::new();
        super::add_oauth_state_cookie(
            &mut jar,
            &key,
            &super::OAuthStateCookie {
                state:     "fabro-test-state".to_string(),
                exp:       (chrono::Utc::now() + chrono::Duration::minutes(30)).timestamp(),
                return_to: Some("/auth/cli/resume".to_string()),
            },
            true,
        );
        let cookie = jar
            .delta()
            .next()
            .expect("private oauth cookie should exist")
            .encoded()
            .to_string();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/auth/callback/github?error=access_denied&error_description=%3Cscript%3Eboom%3C%2Fscript%3E&state=fabro-test-state")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok()),
            Some(
                "/auth/cli/resume?error=access_denied&error_description=Authorization%20denied&state=fabro-test-state"
            )
        );
    }

    #[test]
    fn read_private_session_rejects_v1_cookies() {
        let key = test_cookie_key();
        let mut jar = cookie::CookieJar::new();
        jar.private_mut(&key).add(cookie::Cookie::new(
            super::SESSION_COOKIE_NAME,
            json!({
                "v": 1,
                "login": "dev",
                "auth_method": "dev_token",
                "name": "Development User",
                "email": "dev@localhost",
                "avatar_url": "/logo.svg",
                "user_url": "",
                "identity": null,
                "iat": chrono::Utc::now().timestamp(),
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

    #[test]
    fn read_private_session_rejects_invalid_identity_payload() {
        let key = test_cookie_key();
        let mut jar = cookie::CookieJar::new();
        jar.private_mut(&key).add(cookie::Cookie::new(
            super::SESSION_COOKIE_NAME,
            json!({
                "v": 2,
                "login": "octocat",
                "auth_method": "github",
                "name": "The Octocat",
                "email": "octocat@example.com",
                "avatar_url": "/logo.svg",
                "user_url": "https://github.com/octocat",
                "identity": {
                    "issuer": "",
                    "subject": "12345"
                },
                "iat": chrono::Utc::now().timestamp(),
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
