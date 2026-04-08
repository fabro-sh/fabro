use std::sync::Arc;

use anyhow::{Context, anyhow};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Json, Router, routing::get, routing::post};
use cookie::{Cookie, CookieJar, Expiration, Key, SameSite, time::Duration};
use fabro_types::Settings;
use fabro_types::settings::{ApiAuthStrategy, GitProvider, GitSettings};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{debug, error, info, warn};

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
    webhook_secret: Option<String>,
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
        warn!("OAuth login failed: client_id not configured");
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "GitHub App client_id is not configured"}),
        );
    };
    let Some(web_url) = settings.web.as_ref().map(|web| web.url.clone()) else {
        warn!("OAuth login failed: web.url not configured");
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

    debug!(redirect_uri = %format!("{web_url}/auth/callback/github"), "OAuth login redirecting to GitHub");

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
    let Some(session_key) = state.session_key().await else {
        error!("OAuth callback failed: SESSION_SECRET not configured");
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
        warn!("OAuth callback failed: state mismatch");
        return Redirect::to("/login").into_response();
    }

    let Some(client_id) = settings.client_id().map(str::to_string) else {
        error!("OAuth callback failed: client_id not configured");
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "GitHub App client_id is not configured"}),
        );
    };
    let Some(client_secret) = state.secret_or_env("GITHUB_APP_CLIENT_SECRET") else {
        error!("OAuth callback failed: GITHUB_APP_CLIENT_SECRET not configured");
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

    let allowed_usernames = settings
        .web
        .as_ref()
        .map(|web| web.auth.allowed_usernames.clone())
        .unwrap_or_default();
    if !allowed_usernames.is_empty() && !allowed_usernames.iter().any(|user| user == &profile.login)
    {
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
        login: profile.login.clone(),
        name: profile.name.unwrap_or_else(|| profile.login.clone()),
        email: primary_email,
        avatar_url: profile.avatar_url,
        user_url: format!("https://github.com/{}", profile.login),
        github_id: profile.id,
        exp: (now + chrono::Duration::days(30)).timestamp(),
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
    info!("User logged out");
    let mut jar = CookieJar::new();
    if let Some(key) = state.session_key().await {
        jar.private_mut(&key).remove(
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
    let has_cookie = headers.get(header::COOKIE).is_some();
    let Some(session_key) = state.session_key().await else {
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
    headers: HeaderMap,
    Json(payload): Json<SetupRegisterRequest>,
) -> Response {
    let origin = headers
        .get(header::ORIGIN)
        .or_else(|| headers.get(header::REFERER))
        .and_then(|v| v.to_str().ok())
        .and_then(|s| reqwest::Url::parse(s).ok())
        .map(|url| format!("{}://{}", url.scheme(), url.authority()));

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
        Err(err) => {
            error!(error = %err, "Setup register failed: GitHub manifest conversion request failed");
            return json_response(
                StatusCode::BAD_GATEWAY,
                json!({"error": format!("GitHub manifest conversion failed: {err}")}),
            );
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        error!(status = %status, body = %body, "Setup register failed: GitHub manifest conversion returned error");
        return json_response(
            StatusCode::BAD_GATEWAY,
            json!({"error": format!("GitHub manifest conversion failed: {status}")}),
        );
    }

    let body = match response.text().await {
        Ok(body) => body,
        Err(err) => {
            error!(error = %err, "Setup register failed: could not read conversion response body");
            return json_response(
                StatusCode::BAD_GATEWAY,
                json!({"error": "Failed to read GitHub manifest conversion response"}),
            );
        }
    };
    let data = match serde_json::from_str::<GitHubManifestConversion>(&body) {
        Ok(data) => data,
        Err(err) => {
            error!(error = %err, body = %body, "Setup register failed: could not parse conversion response");
            return json_response(
                StatusCode::BAD_GATEWAY,
                json!({"error": format!("Failed to parse GitHub manifest conversion response: {err}")}),
            );
        }
    };

    let settings_path = state.config_path.clone();

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
    if let Some(ref origin) = origin {
        let web = settings.web.get_or_insert_default();
        web.url.clone_from(origin);
    }

    if let Some(parent) = settings_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let existing = std::fs::read_to_string(&settings_path).unwrap_or_default();
    let mut doc: toml::Value = if existing.is_empty() {
        toml::Value::Table(toml::Table::default())
    } else {
        match toml::from_str(&existing).context("failed to parse existing settings config") {
            Ok(doc) => doc,
            Err(err) => {
                error!(error = %err, path = %settings_path.display(), "Setup register failed: could not parse settings config");
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"error": format!("Failed to parse settings config: {err}")}),
                );
            }
        }
    };
    if let Err(err) = merge_settings_keys(&mut doc, &settings, &git, origin.as_deref()) {
        error!(error = %err, "Setup register failed: could not merge settings");
        return json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": format!("Failed to update settings config: {err}")}),
        );
    }
    if let Err(err) = std::fs::write(
        &settings_path,
        toml::to_string_pretty(&doc).unwrap_or_default(),
    ) {
        error!(error = %err, path = %settings_path.display(), "Setup register failed: could not write settings config");
        return json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": format!("Failed to write settings config: {err}")}),
        );
    }

    let session_secret = hex::encode(rand::random::<[u8; 32]>());
    let mut secret_updates = vec![
        ("SESSION_SECRET", session_secret),
        ("GITHUB_APP_CLIENT_SECRET", data.client_secret.clone()),
        ("GITHUB_APP_PRIVATE_KEY", data.pem.clone()),
    ];
    if let Some(ref webhook_secret) = data.webhook_secret {
        secret_updates.push(("GITHUB_APP_WEBHOOK_SECRET", webhook_secret.clone()));
    }

    {
        let mut store = state.secret_store.write().await;
        for (name, value) in secret_updates {
            if let Err(err) = store.set(name, &value) {
                error!(error = %err, secret = name, "Setup register failed: could not save secret");
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"error": format!("Failed to save secret {name}: {err}")}),
                );
            }
        }
    }

    {
        let mut shared = state.settings.write().expect("settings lock poisoned");
        *shared = settings;
    }

    info!(slug = %data.slug, app_id = %data.id, "GitHub App registered successfully");
    Json(json!({"ok": true})).into_response()
}

fn root_table_mut(doc: &mut toml::Value) -> anyhow::Result<&mut toml::Table> {
    doc.as_table_mut()
        .ok_or_else(|| anyhow!("settings config root is not a table"))
}

fn ensure_table<'a>(table: &'a mut toml::Table, key: &str) -> anyhow::Result<&'a mut toml::Table> {
    table
        .entry(key.to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::default()))
        .as_table_mut()
        .ok_or_else(|| anyhow!("settings config [{key}] is not a table"))
}

fn merge_settings_keys(
    doc: &mut toml::Value,
    settings: &Settings,
    git: &GitSettings,
    origin: Option<&str>,
) -> anyhow::Result<()> {
    let web_url = origin
        .map(str::to_string)
        .or_else(|| settings.web.as_ref().map(|web| web.url.clone()))
        .unwrap_or_else(|| "http://localhost:3000".to_string());
    let allowed = settings
        .web
        .as_ref()
        .map(|web| web.auth.allowed_usernames.clone())
        .unwrap_or_default();
    let api = settings.api.clone().unwrap_or_default();

    let root = root_table_mut(doc)?;
    let web = ensure_table(root, "web")?;
    web.insert("url".to_string(), toml::Value::String(web_url.clone()));
    let auth = ensure_table(web, "auth")?;
    auth.insert(
        "provider".to_string(),
        toml::Value::String("github".to_string()),
    );
    auth.insert(
        "allowed_usernames".to_string(),
        toml::Value::Array(allowed.into_iter().map(toml::Value::String).collect()),
    );

    let base_url = format!("{web_url}/api/v1");
    let api_table = ensure_table(root, "api")?;
    api_table.insert("base_url".to_string(), toml::Value::String(base_url));
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

    let git_table = ensure_table(root, "git")?;
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

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::merge_settings_keys;
    use fabro_types::Settings;

    #[test]
    fn merge_settings_keys_preserves_unrelated_git_nested_keys() {
        let mut doc: toml::Value = toml::from_str(
            r#"
[git]
provider = "github"

[git.author]
name = "fabro"
email = "fabro@example.com"

[git.webhooks]
strategy = "tailscale_funnel"
"#,
        )
        .unwrap();

        let mut settings = Settings::default();
        settings.web.get_or_insert_default().auth.allowed_usernames = vec!["alice".to_string()];
        settings.git.get_or_insert_default().provider = fabro_config::server::GitProvider::Github;
        settings.git.get_or_insert_default().app_id = Some("123".to_string());
        settings.git.get_or_insert_default().client_id = Some("abc".to_string());
        settings.git.get_or_insert_default().slug = Some("fabro".to_string());

        merge_settings_keys(&mut doc, &settings, settings.git.as_ref().unwrap(), None).unwrap();

        let git = doc.get("git").and_then(toml::Value::as_table).unwrap();
        assert_eq!(git.get("app_id").and_then(toml::Value::as_str), Some("123"));
        let author = git.get("author").and_then(toml::Value::as_table).unwrap();
        assert_eq!(
            author.get("name").and_then(toml::Value::as_str),
            Some("fabro")
        );
        let webhooks = git.get("webhooks").and_then(toml::Value::as_table).unwrap();
        assert_eq!(
            webhooks.get("strategy").and_then(toml::Value::as_str),
            Some("tailscale_funnel")
        );
    }

    #[test]
    fn merge_settings_keys_preserves_unrelated_top_level_sections() {
        let mut doc: toml::Value = toml::from_str(
            r#"
[exec]
provider = "anthropic"

[server]
target = "https://fabro.example.com/api/v1"
"#,
        )
        .unwrap();

        let mut settings = Settings::default();
        settings.web.get_or_insert_default().auth.allowed_usernames = vec!["alice".to_string()];
        settings.git.get_or_insert_default().provider = fabro_config::server::GitProvider::Github;
        settings.git.get_or_insert_default().app_id = Some("123".to_string());
        settings.git.get_or_insert_default().client_id = Some("abc".to_string());
        settings.git.get_or_insert_default().slug = Some("fabro".to_string());

        merge_settings_keys(&mut doc, &settings, settings.git.as_ref().unwrap(), None).unwrap();

        assert_eq!(
            doc.get("exec")
                .and_then(toml::Value::as_table)
                .and_then(|exec| exec.get("provider"))
                .and_then(toml::Value::as_str),
            Some("anthropic")
        );
        assert_eq!(
            doc.get("server")
                .and_then(toml::Value::as_table)
                .and_then(|server| server.get("target"))
                .and_then(toml::Value::as_str),
            Some("https://fabro.example.com/api/v1")
        );
    }
}
