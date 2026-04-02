use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Json, Router, routing::get, routing::post};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use cookie::{Cookie, CookieJar, Expiration, Key, SameSite, time::Duration};
use fabro_types::Settings;
use fabro_types::settings::{ApiAuthStrategy, GitProvider, GitSettings};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::server::AppState;

pub const SESSION_COOKIE_NAME: &str = "__fabro_session";
const OAUTH_STATE_COOKIE_NAME: &str = "fabro_oauth_state";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionCookie {
    pub login: String,
    pub name: String,
    pub email: String,
    pub avatar_url: String,
    pub user_url: String,
    pub github_id: i64,
    pub exp: i64,
}

#[derive(Deserialize)]
struct OAuthCallbackParams {
    code: String,
    state: String,
}

#[derive(Deserialize)]
struct SetupRegisterRequest {
    code: String,
}

#[derive(Deserialize)]
struct DemoToggleRequest {
    enabled: bool,
}

#[derive(Serialize)]
struct SetupStatusResponse {
    configured: bool,
}

#[derive(Serialize)]
struct AuthMeResponse {
    user: SessionUser,
    provider: &'static str,
    #[serde(rename = "demoMode")]
    demo_mode: bool,
    features: serde_json::Value,
}

#[derive(Serialize)]
struct SessionUser {
    login: String,
    name: String,
    email: String,
    #[serde(rename = "avatarUrl")]
    avatar_url: String,
    #[serde(rename = "userUrl")]
    user_url: String,
}

#[derive(Deserialize)]
struct GitHubTokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct GitHubUser {
    id: i64,
    login: String,
    name: Option<String>,
    avatar_url: String,
}

#[derive(Deserialize)]
struct GitHubEmail {
    email: String,
    primary: bool,
    verified: bool,
}

#[derive(Deserialize)]
struct GitHubManifestConversion {
    id: i64,
    slug: String,
    client_id: String,
    client_secret: String,
    webhook_secret: String,
    pem: String,
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/login/github", get(login_github))
        .route("/callback/github", get(callback_github))
        .route("/logout", post(logout))
}

pub fn api_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/auth/me", get(auth_me))
        .route("/setup/register", post(setup_register))
        .route("/setup/status", get(setup_status))
        .route("/demo/toggle", post(toggle_demo))
}

pub fn parse_cookie_header(headers: &HeaderMap) -> CookieJar {
    let mut jar = CookieJar::new();
    if let Some(raw) = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
    {
        for part in raw.split(';') {
            if let Ok(cookie) = Cookie::parse(part.trim().to_string()) {
                jar.add_original(cookie.into_owned());
            }
        }
    }
    jar
}

pub fn session_key_from_env() -> Option<Key> {
    std::env::var("SESSION_SECRET")
        .ok()
        .map(|secret| Key::derive_from(secret.as_bytes()))
}

pub fn read_private_session(headers: &HeaderMap, key: &Key) -> Option<SessionCookie> {
    let jar = parse_cookie_header(headers);
    let cookie = jar.private(key).get(SESSION_COOKIE_NAME)?;
    serde_json::from_str(cookie.value()).ok()
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

fn features_json(settings: &Settings) -> serde_json::Value {
    let features = settings.features.clone().unwrap_or_default();
    json!({
        "session_sandboxes": features.session_sandboxes,
        "retros": features.retros,
    })
}

async fn login_github(State(state): State<Arc<AppState>>) -> Response {
    let settings = state
        .settings
        .read()
        .expect("settings lock poisoned")
        .clone();
    let Some(client_id) = settings.client_id().map(str::to_string) else {
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "GitHub App client_id is not configured"}),
        );
    };
    let Some(web_url) = settings.web.as_ref().map(|web| web.url.clone()) else {
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "web.url is not configured"}),
        );
    };

    let state_token = format!("fabro-{}", ulid::Ulid::new());
    let authorize_url = reqwest::Url::parse_with_params(
        "https://github.com/login/oauth/authorize",
        &[
            ("client_id", client_id.as_str()),
            ("redirect_uri", &format!("{web_url}/auth/callback/github")),
            ("scope", "read:user user:email"),
            ("state", state_token.as_str()),
        ],
    )
    .expect("GitHub authorize URL should be valid");

    let mut jar = CookieJar::new();
    jar.add(
        Cookie::build((OAUTH_STATE_COOKIE_NAME, state_token))
            .path("/")
            .http_only(true)
            .same_site(SameSite::Lax)
            .max_age(Duration::minutes(10))
            .build(),
    );
    let mut response = Redirect::to(authorize_url.as_str()).into_response();
    append_jar_delta(response.headers_mut(), &jar);
    response
}

async fn callback_github(
    State(state): State<Arc<AppState>>,
    Query(params): Query<OAuthCallbackParams>,
    headers: HeaderMap,
) -> Response {
    let Some(session_key) = state.session_key.clone() else {
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "SESSION_SECRET is not configured"}),
        );
    };
    let settings = state
        .settings
        .read()
        .expect("settings lock poisoned")
        .clone();
    let cookie_jar = parse_cookie_header(&headers);
    let stored_state = cookie_jar.get(OAUTH_STATE_COOKIE_NAME).map(Cookie::value);
    if stored_state != Some(params.state.as_str()) {
        return Redirect::to("/login").into_response();
    }

    let Some(client_id) = settings.client_id().map(str::to_string) else {
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "GitHub App client_id is not configured"}),
        );
    };
    let Ok(client_secret) = std::env::var("GITHUB_APP_CLIENT_SECRET") else {
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "GITHUB_APP_CLIENT_SECRET is not configured"}),
        );
    };
    let web_url = settings.web.as_ref().map_or_else(
        || "http://localhost:3000".to_string(),
        |web| web.url.clone(),
    );

    let http = reqwest::Client::new();
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
                Err(_) => {
                    return json_response(
                        StatusCode::BAD_GATEWAY,
                        json!({"error": "Failed to parse GitHub token response"}),
                    );
                }
            }
        }
        Ok(response) => {
            return json_response(
                StatusCode::BAD_GATEWAY,
                json!({"error": format!("GitHub token exchange failed: {}", response.status())}),
            );
        }
        Err(error) => {
            return json_response(
                StatusCode::BAD_GATEWAY,
                json!({"error": format!("GitHub token exchange failed: {error}")}),
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
            Err(_) => {
                return json_response(
                    StatusCode::BAD_GATEWAY,
                    json!({"error": "Failed to parse GitHub user response"}),
                );
            }
        },
        Ok(response) => {
            return json_response(
                StatusCode::BAD_GATEWAY,
                json!({"error": format!("GitHub user lookup failed: {}", response.status())}),
            );
        }
        Err(error) => {
            return json_response(
                StatusCode::BAD_GATEWAY,
                json!({"error": format!("GitHub user lookup failed: {error}")}),
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

    let allowed_usernames = settings
        .web
        .as_ref()
        .map(|web| web.auth.allowed_usernames.clone())
        .unwrap_or_default();
    if !allowed_usernames.is_empty() && !allowed_usernames.iter().any(|user| user == &profile.login)
    {
        return Redirect::to("/login?error=unauthorized").into_response();
    }

    let primary_email = emails
        .iter()
        .find(|email| email.primary && email.verified)
        .map(|email| email.email.clone())
        .unwrap_or_default();
    let now = chrono::Utc::now();
    let session = SessionCookie {
        login: profile.login.clone(),
        name: profile.name.unwrap_or_else(|| profile.login.clone()),
        email: primary_email,
        avatar_url: profile.avatar_url,
        user_url: format!("https://github.com/{}", profile.login),
        github_id: profile.id,
        exp: (now + chrono::Duration::days(30)).timestamp(),
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
        .secure(false)
        .max_age(Duration::days(30))
        .build(),
    );
    jar.add(
        Cookie::build((OAUTH_STATE_COOKIE_NAME, ""))
            .path("/")
            .http_only(true)
            .expires(Expiration::Session)
            .max_age(Duration::seconds(0))
            .build(),
    );
    let mut response = Redirect::to("/start").into_response();
    append_jar_delta(response.headers_mut(), &jar);
    response
}

async fn logout(State(state): State<Arc<AppState>>) -> Response {
    let mut jar = CookieJar::new();
    if let Some(key) = &state.session_key {
        jar.private_mut(key).remove(
            Cookie::build((SESSION_COOKIE_NAME, ""))
                .path("/")
                .http_only(true)
                .build(),
        );
    }
    let mut response = Redirect::to("/login").into_response();
    append_jar_delta(response.headers_mut(), &jar);
    response
}

async fn auth_me(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let Some(session_key) = &state.session_key else {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "Unauthorized"}));
    };
    let Some(session) = read_private_session(&headers, session_key) else {
        return json_response(StatusCode::UNAUTHORIZED, json!({"error": "Unauthorized"}));
    };

    let settings = state
        .settings
        .read()
        .expect("settings lock poisoned")
        .clone();
    let demo_mode = parse_cookie_header(&headers)
        .get("fabro-demo")
        .is_some_and(|cookie| cookie.value() == "1");
    Json(AuthMeResponse {
        user: SessionUser {
            login: session.login,
            name: session.name,
            email: session.email,
            avatar_url: session.avatar_url,
            user_url: session.user_url,
        },
        provider: "github",
        demo_mode,
        features: features_json(&settings),
    })
    .into_response()
}

async fn setup_status(State(state): State<Arc<AppState>>) -> Response {
    let settings = state
        .settings
        .read()
        .expect("settings lock poisoned")
        .clone();
    let configured = settings
        .git
        .as_ref()
        .is_some_and(|git| git.client_id.is_some());
    Json(SetupStatusResponse { configured }).into_response()
}

async fn toggle_demo(Json(payload): Json<DemoToggleRequest>) -> Response {
    let mut jar = CookieJar::new();
    jar.add(
        Cookie::build(("fabro-demo", if payload.enabled { "1" } else { "0" }))
            .path("/")
            .same_site(SameSite::Lax)
            .max_age(Duration::days(365))
            .build(),
    );
    let mut response = Json(json!({ "enabled": payload.enabled })).into_response();
    append_jar_delta(response.headers_mut(), &jar);
    response
}

async fn setup_register(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SetupRegisterRequest>,
) -> Response {
    let http = reqwest::Client::new();
    let response = match http
        .post(format!(
            "https://api.github.com/app-manifests/{}/conversions",
            payload.code
        ))
        .header(header::ACCEPT, "application/vnd.github+json")
        .header(header::USER_AGENT, "fabro-server")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return json_response(
                StatusCode::BAD_GATEWAY,
                json!({"error": format!("GitHub manifest conversion failed: {error}")}),
            );
        }
    };

    if !response.status().is_success() {
        return json_response(
            StatusCode::BAD_GATEWAY,
            json!({"error": format!("GitHub manifest conversion failed: {}", response.status())}),
        );
    }

    let Ok(data) = response.json::<GitHubManifestConversion>().await else {
        return json_response(
            StatusCode::BAD_GATEWAY,
            json!({"error": "Failed to parse GitHub manifest conversion response"}),
        );
    };

    let settings_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".fabro")
        .join("server.toml");
    let env_path = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".env");

    let mut settings = state
        .settings
        .read()
        .expect("settings lock poisoned")
        .clone();
    let mut git = settings.git.clone().unwrap_or_default();
    git.provider = GitProvider::Github;
    git.app_id = Some(data.id.to_string());
    git.client_id = Some(data.client_id.clone());
    git.slug = Some(data.slug.clone());
    settings.git = Some(git.clone());

    if let Some(parent) = settings_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let toml = build_server_toml(&settings, &git);
    if let Err(error) = std::fs::write(&settings_path, toml) {
        return json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": format!("Failed to write server config: {error}")}),
        );
    }

    let session_secret = hex::encode(rand::random::<[u8; 32]>());
    let env_updates = BTreeMap::from([
        ("SESSION_SECRET".to_string(), session_secret),
        (
            "GITHUB_APP_CLIENT_SECRET".to_string(),
            data.client_secret.clone(),
        ),
        (
            "GITHUB_APP_WEBHOOK_SECRET".to_string(),
            data.webhook_secret.clone(),
        ),
        (
            "GITHUB_APP_PRIVATE_KEY".to_string(),
            STANDARD.encode(data.pem),
        ),
    ]);
    if let Err(error) = write_env_file(&env_path, &env_updates) {
        return json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": format!("Failed to write .env: {error}")}),
        );
    }

    {
        let mut shared = state.settings.write().expect("settings lock poisoned");
        *shared = settings;
    }

    Json(json!({"ok": true, "restart_required": true})).into_response()
}

fn build_server_toml(settings: &Settings, git: &GitSettings) -> String {
    let web_url = settings.web.as_ref().map_or_else(
        || "http://localhost:3000".to_string(),
        |web| web.url.clone(),
    );
    let allowed = settings
        .web
        .as_ref()
        .map(|web| web.auth.allowed_usernames.clone())
        .unwrap_or_default();
    let api = settings.api.clone().unwrap_or_default();

    let mut value = toml::Table::new();
    value.insert(
        "web".to_string(),
        toml::Value::Table({
            let mut web = toml::Table::new();
            web.insert("url".to_string(), toml::Value::String(web_url));
            web.insert(
                "auth".to_string(),
                toml::Value::Table({
                    let mut auth = toml::Table::new();
                    auth.insert(
                        "provider".to_string(),
                        toml::Value::String("github".to_string()),
                    );
                    auth.insert(
                        "allowed_usernames".to_string(),
                        toml::Value::Array(allowed.into_iter().map(toml::Value::String).collect()),
                    );
                    auth
                }),
            );
            web
        }),
    );
    value.insert(
        "api".to_string(),
        toml::Value::Table({
            let mut api_table = toml::Table::new();
            api_table.insert("base_url".to_string(), toml::Value::String(api.base_url));
            api_table.insert(
                "authentication_strategies".to_string(),
                toml::Value::Array(
                    api.authentication_strategies
                        .iter()
                        .map(|strategy| match strategy {
                            ApiAuthStrategy::Jwt => "jwt",
                            ApiAuthStrategy::Mtls => "mtls",
                        })
                        .map(|value| toml::Value::String(value.to_string()))
                        .collect(),
                ),
            );
            api_table
        }),
    );
    value.insert(
        "git".to_string(),
        toml::Value::Table({
            let mut git_table = toml::Table::new();
            git_table.insert(
                "provider".to_string(),
                toml::Value::String("github".to_string()),
            );
            git_table.insert(
                "app_id".to_string(),
                toml::Value::String(git.app_id.clone().unwrap_or_default()),
            );
            git_table.insert(
                "client_id".to_string(),
                toml::Value::String(git.client_id.clone().unwrap_or_default()),
            );
            git_table.insert(
                "slug".to_string(),
                toml::Value::String(git.slug.clone().unwrap_or_default()),
            );
            git_table
        }),
    );
    toml::to_string(&value).unwrap_or_default()
}

fn write_env_file(path: &PathBuf, updates: &BTreeMap<String, String>) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut merged = BTreeMap::new();
    for line in existing.lines() {
        if let Some((key, value)) = line.split_once('=') {
            merged.insert(key.trim().to_string(), value.to_string());
        }
    }
    merged.extend(
        updates
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    let body = merged
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(path, format!("{body}\n"))
}
