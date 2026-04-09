use std::sync::Arc;

use anyhow::{Context, anyhow};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Json, Router, routing::get, routing::post};
use cookie::{Cookie, CookieJar, Expiration, Key, SameSite, time::Duration};
use fabro_types::settings::{InterpString, SettingsFile};
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

fn features_json(settings: &SettingsFile) -> serde_json::Value {
    let features = settings.features.as_ref();
    let session_sandboxes = features.and_then(|f| f.session_sandboxes).unwrap_or(false);
    // Retros in v2 live under `run.execution.retros` (positive form) rather
    // than the top-level features stanza.
    let retros = settings
        .run_execution()
        .and_then(|e| e.retros)
        .unwrap_or(false);
    json!({
        "session_sandboxes": session_sandboxes,
        "retros": retros,
    })
}

async fn login_github(State(state): State<Arc<AppState>>) -> Response {
    let settings = state
        .settings
        .read()
        .expect("settings lock poisoned")
        .clone();
    let Some(client_id) = settings.github_client_id_str() else {
        warn!("OAuth login failed: client_id not configured");
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "GitHub App client_id is not configured"}),
        );
    };
    let Some(web_url) = settings
        .server_web()
        .and_then(|w| w.url.as_ref())
        .map(InterpString::as_source)
    else {
        warn!("OAuth login failed: server.web.url not configured");
        return json_response(
            StatusCode::CONFLICT,
            json!({"error": "server.web.url is not configured"}),
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

    let Some(client_id) = settings.github_client_id_str() else {
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
    let web_url = settings
        .server_web()
        .and_then(|w| w.url.as_ref())
        .map_or_else(
            || "http://localhost:3000".to_string(),
            InterpString::as_source,
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
        .server
        .as_ref()
        .and_then(|s| s.auth.as_ref())
        .and_then(|a| a.web.as_ref())
        .map(|w| w.allowed_usernames.clone())
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
    let configured = settings.github_client_id_str().is_some();
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

    // Build a v2 settings_path edit in place. This used to bridge back to
    // the legacy flat shape and emit v1 TOML; the v2 parser hard-rejects
    // the v1 top-level keys, so this was already broken. Write v2 TOML
    // using `merge_settings_keys` against the raw TOML document.
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
    if let Err(err) = merge_settings_keys(&mut doc, &data, origin.as_deref()) {
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

    // Re-parse the freshly-written settings file and swap it into the
    // in-memory state. Stage 6.6 may split this differently when the web
    // setup flow is reworked, but for now a round-trip through
    // `ConfigLayer::load` keeps the live state consistent with disk.
    if let Ok(reloaded) = fabro_config::ConfigLayer::load(&settings_path) {
        let mut shared = state.settings.write().expect("settings lock poisoned");
        *shared = reloaded.into();
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
    data: &GitHubManifestConversion,
    origin: Option<&str>,
) -> anyhow::Result<()> {
    let web_url = origin.map_or_else(|| "http://localhost:3000".to_string(), str::to_string);

    let root = root_table_mut(doc)?;
    // Make sure the freshly-written file is a valid v2 file.
    root.insert("_version".to_string(), toml::Value::Integer(1));

    let server = ensure_table(root, "server")?;
    let web = ensure_table(server, "web")?;
    web.insert("enabled".to_string(), toml::Value::Boolean(true));
    web.insert("url".to_string(), toml::Value::String(web_url));

    let auth = ensure_table(server, "auth")?;
    let auth_web = ensure_table(auth, "web")?;
    let _ = auth_web;
    let auth_api = ensure_table(auth, "api")?;
    let _jwt = ensure_table(auth_api, "jwt")?;

    let integrations = ensure_table(server, "integrations")?;
    let github = ensure_table(integrations, "github")?;
    github.insert(
        "app_id".to_string(),
        toml::Value::String(data.id.to_string()),
    );
    github.insert(
        "client_id".to_string(),
        toml::Value::String(data.client_id.clone()),
    );
    github.insert("slug".to_string(), toml::Value::String(data.slug.clone()));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{GitHubManifestConversion, merge_settings_keys};

    fn sample_conversion() -> GitHubManifestConversion {
        GitHubManifestConversion {
            id: 123,
            slug: "fabro".to_string(),
            client_id: "abc".to_string(),
            client_secret: "shh".to_string(),
            pem: String::new(),
            webhook_secret: None,
        }
    }

    #[test]
    fn merge_settings_keys_writes_v2_server_integrations_github() {
        let mut doc: toml::Value =
            toml::from_str("_version = 1\n").expect("empty v2 doc should parse");
        merge_settings_keys(&mut doc, &sample_conversion(), Some("https://example.test")).unwrap();

        let github = doc
            .get("server")
            .and_then(toml::Value::as_table)
            .and_then(|s| s.get("integrations"))
            .and_then(toml::Value::as_table)
            .and_then(|i| i.get("github"))
            .and_then(toml::Value::as_table)
            .expect("server.integrations.github should exist");
        assert_eq!(
            github.get("app_id").and_then(toml::Value::as_str),
            Some("123")
        );
        assert_eq!(
            github.get("slug").and_then(toml::Value::as_str),
            Some("fabro")
        );

        let web = doc
            .get("server")
            .and_then(toml::Value::as_table)
            .and_then(|s| s.get("web"))
            .and_then(toml::Value::as_table)
            .expect("server.web should exist");
        assert_eq!(
            web.get("url").and_then(toml::Value::as_str),
            Some("https://example.test")
        );
    }
}
