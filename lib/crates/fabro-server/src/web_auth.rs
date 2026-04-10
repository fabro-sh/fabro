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
    let retros = fabro_config::resolve_run_from_file(settings)
        .map(|settings| settings.execution.retros)
        .unwrap_or(false);
    json!({
        "session_sandboxes": session_sandboxes,
        "retros": retros,
    })
}

fn resolve_interp(value: &InterpString) -> anyhow::Result<String> {
    value
        .resolve(|name| std::env::var(name).ok())
        .map(|resolved| resolved.value)
        .map_err(anyhow::Error::from)
}

async fn login_github(State(state): State<Arc<AppState>>) -> Response {
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
    let settings = state.server_settings();
    let cookie_jar = parse_cookie_header(&headers);
    let stored_state = cookie_jar.get(OAUTH_STATE_COOKIE_NAME).map(Cookie::value);
    if stored_state != Some(params.state.as_str()) {
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
    let Some(client_secret) = state.secret_or_env("GITHUB_APP_CLIENT_SECRET") else {
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

    let allowed_usernames = settings.auth.web.allowed_usernames.clone();
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
    let configured = state
        .server_settings()
        .integrations
        .github
        .client_id
        .is_some();
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

    // Edit the settings file in place via `toml_edit::DocumentMut`, which
    // preserves existing comments, whitespace, and key ordering. The value-
    // tree parser (`toml::Value`) would strip all of that on round-trip.
    if let Some(parent) = settings_path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            error!(error = %err, path = %parent.display(), "Setup register failed: could not create settings parent directory");
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": format!("Failed to create settings directory: {err}")}),
            );
        }
    }
    let existing = std::fs::read_to_string(&settings_path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = if existing.is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        match existing
            .parse::<toml_edit::DocumentMut>()
            .context("failed to parse existing settings config")
        {
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
    if let Err(err) = std::fs::write(&settings_path, doc.to_string()) {
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
    // in-memory state so subsequent OAuth requests see the new GitHub
    // App credentials without a server restart.
    match state.reload_settings_from_disk() {
        Ok(()) => {}
        Err(err) => {
            error!(error = %err, path = %settings_path.display(), "Setup register failed: could not reload written settings config");
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": format!("Failed to reload settings config after write: {err}")}),
            );
        }
    }

    info!(slug = %data.slug, app_id = %data.id, "GitHub App registered successfully");
    Json(json!({"ok": true})).into_response()
}

/// Walk dotted `path` into `doc`, creating missing intermediate tables,
/// and return a mutable reference to the terminal table.
///
/// Uses `toml_edit`'s [`toml_edit::Entry::or_insert`] so existing tables
/// keep their comments, ordering, and any sibling keys untouched.
fn ensure_nested_table<'a>(
    doc: &'a mut toml_edit::DocumentMut,
    path: &[&str],
) -> anyhow::Result<&'a mut toml_edit::Table> {
    let mut current: &mut toml_edit::Table = doc.as_table_mut();
    for segment in path {
        let next = current
            .entry(segment)
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
        current = next
            .as_table_mut()
            .ok_or_else(|| anyhow!("settings config [{segment}] is not a table"))?;
    }
    Ok(current)
}

/// Set a scalar value on `table[key]`, preserving any key-level decor
/// (leading comments, blank lines) that was attached to the existing entry.
///
/// `toml_edit`'s default `Table::insert` replaces the entry wholesale and
/// drops its prefix decoration, which would strip a top-of-file comment
/// attached to a key we're updating. Copying the decor forward keeps the
/// user's formatting intact.
fn set_preserving_decor(table: &mut toml_edit::Table, key: &str, value: toml_edit::Item) {
    let preserved_decor = table.key(key).map(|existing| existing.leaf_decor().clone());
    table.insert(key, value);
    if let (Some(decor), Some(mut updated)) = (preserved_decor, table.key_mut(key)) {
        *updated.leaf_decor_mut() = decor;
    }
}

fn merge_settings_keys(
    doc: &mut toml_edit::DocumentMut,
    data: &GitHubManifestConversion,
    origin: Option<&str>,
) -> anyhow::Result<()> {
    let web_url = origin.map_or_else(|| "http://localhost:3000".to_string(), str::to_string);

    // Make sure the freshly-written file is a valid v2 file. `_version` is
    // always `1` at the moment, so skip the write entirely if it's already
    // there -- otherwise we'd trample any top-of-file comment attached to
    // the key.
    let root = doc.as_table_mut();
    if !root.contains_key("_version") {
        root.insert("_version", toml_edit::value(1_i64));
    }

    let web = ensure_nested_table(doc, &["server", "web"])?;
    set_preserving_decor(web, "enabled", toml_edit::value(true));
    set_preserving_decor(web, "url", toml_edit::value(web_url));

    // Ensure the auth subtrees exist so a freshly-registered GitHub App
    // resolves `[server.auth.web]` / `[server.auth.api.jwt]` strategies on
    // the next startup without a second manual edit.
    ensure_nested_table(doc, &["server", "auth", "web"])?;
    ensure_nested_table(doc, &["server", "auth", "api", "jwt"])?;

    let github = ensure_nested_table(doc, &["server", "integrations", "github"])?;
    set_preserving_decor(github, "app_id", toml_edit::value(data.id.to_string()));
    set_preserving_decor(
        github,
        "client_id",
        toml_edit::value(data.client_id.clone()),
    );
    set_preserving_decor(github, "slug", toml_edit::value(data.slug.clone()));

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

    fn parse_doc(source: &str) -> toml_edit::DocumentMut {
        source
            .parse::<toml_edit::DocumentMut>()
            .expect("fixture should parse as TOML")
    }

    #[test]
    fn merge_settings_keys_writes_v2_server_integrations_github() {
        let mut doc = parse_doc("_version = 1\n");
        merge_settings_keys(&mut doc, &sample_conversion(), Some("https://example.test")).unwrap();

        let github = doc["server"]["integrations"]["github"]
            .as_table()
            .expect("server.integrations.github should exist");
        assert_eq!(github["app_id"].as_str(), Some("123"));
        assert_eq!(github["slug"].as_str(), Some("fabro"));
        assert_eq!(github["client_id"].as_str(), Some("abc"));

        let web = doc["server"]["web"]
            .as_table()
            .expect("server.web should exist");
        assert_eq!(web["url"].as_str(), Some("https://example.test"));
        assert_eq!(web["enabled"].as_bool(), Some(true));

        // Re-parse the emitted document to prove it round-trips into a
        // valid v2 `SettingsFile`.
        let emitted = doc.to_string();
        let file = fabro_config::ConfigLayer::parse(&emitted)
            .expect("merged output should parse as a v2 SettingsFile");
        let server = file
            .as_v2()
            .server
            .as_ref()
            .expect("[server] should be present");
        let integrations = server
            .integrations
            .as_ref()
            .expect("[server.integrations] should be present");
        let github = integrations
            .github
            .as_ref()
            .expect("[server.integrations.github] should be present");
        assert_eq!(
            github.app_id.as_ref().map(|s| s.as_source()),
            Some("123".to_string())
        );
    }

    #[test]
    fn merge_settings_keys_preserves_comments_and_unrelated_keys() {
        let existing = r##"# Top-of-file comment explaining the settings layout.
_version = 1

# Storage root comment — should survive the edit.
[server.storage]
root = "/srv/fabro-data"

# A pre-existing integration that is NOT github.
[server.integrations.slack]
default_channel = "#ops"

[run.model]
provider = "anthropic"
name = "claude-sonnet"
"##;
        let mut doc = parse_doc(existing);
        merge_settings_keys(
            &mut doc,
            &sample_conversion(),
            Some("https://fabro.example"),
        )
        .unwrap();

        let emitted = doc.to_string();

        // Comments must survive the round-trip.
        assert!(
            emitted.contains("# Top-of-file comment explaining the settings layout."),
            "top-of-file comment was stripped:\n{emitted}"
        );
        assert!(
            emitted.contains("# Storage root comment — should survive the edit."),
            "inline table comment was stripped:\n{emitted}"
        );
        assert!(
            emitted.contains("# A pre-existing integration that is NOT github."),
            "sibling-table comment was stripped:\n{emitted}"
        );

        // Unrelated keys must still be intact.
        assert!(
            emitted.contains(r#"root = "/srv/fabro-data""#),
            "server.storage.root was lost:\n{emitted}"
        );
        assert!(
            emitted.contains(r##"default_channel = "#ops""##),
            "server.integrations.slack.default_channel was lost:\n{emitted}"
        );
        assert!(
            emitted.contains(r#"provider = "anthropic""#),
            "run.model.provider was lost:\n{emitted}"
        );

        // And the new keys must be present.
        assert!(
            emitted.contains(r#"app_id = "123""#),
            "server.integrations.github.app_id missing:\n{emitted}"
        );
        assert!(
            emitted.contains(r#"url = "https://fabro.example""#),
            "server.web.url missing:\n{emitted}"
        );

        // Finally, the whole thing must still parse as a valid v2
        // SettingsFile.
        fabro_config::ConfigLayer::parse(&emitted)
            .expect("merged output should still parse as v2 after the edit");
    }
}
