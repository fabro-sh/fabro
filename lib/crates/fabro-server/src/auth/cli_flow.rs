use std::net;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use cookie::time::Duration;
use cookie::{Cookie, CookieJar, Key, SameSite};
use fabro_types::RunAuthMethod;
use fabro_types::settings::ServerAuthMethod;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use rand::TryRngCore;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use url::{Host, Url};

use crate::auth::{self, AuthCode, ConsumeOutcome, JwtSubject, RefreshToken};
use crate::jwt_auth::{AuthMode, ConfiguredAuth};
use crate::server::AppState;
use crate::web_auth::{SessionCookie, read_private_session};

const CLI_FLOW_COOKIE_NAME: &str = "fabro_cli_flow";
const QUERY_VALUE_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC.remove(b'_').remove(b'-');
const ACCESS_TOKEN_TTL_MINUTES: i64 = 10;
const REFRESH_TOKEN_TTL_DAYS: i64 = 30;
const REFRESH_TOKEN_PREFIX: &str = "fabro_refresh_";
const GITHUB_NOT_CONFIGURED: &str = "GitHub login is not configured for this server.";
const INVALID_REDIRECT_URI: &str = "The provided redirect URI is not valid for CLI login.";
const INVALID_OR_MISSING_STATE: &str = "The login state is missing or invalid.";
const MISSING_FLOW_COOKIE: &str = "Your login session has expired. Please start again.";
const INVALID_CONFIRMATION_REQUEST: &str =
    "This CLI login confirmation is invalid. Return to the Fabro login page and try again.";

#[derive(Serialize)]
struct OAuthErrorResponse<'a> {
    error:             &'a str,
    error_description: &'a str,
}

#[derive(Deserialize)]
struct CliStartParams {
    redirect_uri:          Option<String>,
    state:                 Option<String>,
    code_challenge:        Option<String>,
    code_challenge_method: Option<String>,
}

#[derive(Deserialize)]
struct CliResumeParams {
    error: Option<String>,
}

#[derive(Deserialize)]
struct CliTokenRequest {
    grant_type:    Option<String>,
    code:          Option<String>,
    code_verifier: Option<String>,
    redirect_uri:  Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CliFlowCookie {
    redirect_uri:   String,
    state:          String,
    code_challenge: String,
}

#[derive(Serialize)]
struct CliAuthSubjectResponse {
    idp_issuer:  String,
    idp_subject: String,
    login:       String,
    name:        String,
    email:       String,
}

#[derive(Serialize)]
struct CliTokenResponse {
    access_token:             String,
    access_token_expires_at:  chrono::DateTime<chrono::Utc>,
    refresh_token:            String,
    refresh_token_expires_at: chrono::DateTime<chrono::Utc>,
    subject:                  CliAuthSubjectResponse,
}

pub(crate) fn web_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/cli/start", get(start))
        .route("/cli/resume", get(resume).post(confirm_resume))
        .route("/cli/token", post(token))
        .route("/cli/refresh", post(refresh))
        .route("/cli/logout", post(logout))
}

async fn start(
    State(state): State<Arc<AppState>>,
    Extension(auth_mode): Extension<AuthMode>,
    Query(params): Query<CliStartParams>,
    headers: HeaderMap,
) -> Response {
    let Some(redirect_uri) = params
        .redirect_uri
        .as_deref()
        .and_then(canonical_loopback_redirect_uri)
    else {
        return static_error_page(INVALID_REDIRECT_URI);
    };

    let Some(state_token) = params.state.as_deref() else {
        return static_error_page(INVALID_OR_MISSING_STATE);
    };
    if !valid_state_token(state_token) {
        return static_error_page(INVALID_OR_MISSING_STATE);
    }

    if !github_enabled(&auth_mode) {
        return redirect_with_error(
            &redirect_uri,
            state_token,
            "github_not_configured",
            "GitHub authentication is not enabled on this server",
        );
    }

    let Some(session_key) = state.session_key() else {
        return redirect_with_error(
            &redirect_uri,
            state_token,
            "server_error",
            "SESSION_SECRET is not configured on this server",
        );
    };

    let Some(code_challenge) = params.code_challenge.as_deref() else {
        return redirect_with_error(
            &redirect_uri,
            state_token,
            "invalid_request",
            "Invalid PKCE parameters",
        );
    };
    if params.code_challenge_method.as_deref() != Some("S256") {
        return redirect_with_error(
            &redirect_uri,
            state_token,
            "invalid_request",
            "Invalid PKCE parameters",
        );
    }
    let session = read_private_session(&headers, &session_key);

    let secure = session_cookie_secure(state.as_ref());
    let mut jar = CookieJar::new();
    add_cli_flow_cookie(
        &mut jar,
        &session_key,
        &CliFlowCookie {
            redirect_uri,
            state: state_token.to_string(),
            code_challenge: code_challenge.to_string(),
        },
        secure,
    );
    let redirect_target = if eligible_session(session.as_ref()).is_some() {
        "/auth/cli/resume"
    } else {
        "/auth/login/github?return_to=/auth/cli/resume"
    };
    let mut response = Redirect::to(redirect_target).into_response();
    append_jar_delta(response.headers_mut(), &jar);
    response
}

async fn resume(
    State(state): State<Arc<AppState>>,
    Extension(auth_mode): Extension<AuthMode>,
    Query(params): Query<CliResumeParams>,
    headers: HeaderMap,
) -> Response {
    if !github_enabled(&auth_mode) {
        return static_error_page(GITHUB_NOT_CONFIGURED);
    }

    let Some(session_key) = state.session_key() else {
        return static_error_page(GITHUB_NOT_CONFIGURED);
    };
    let Some(flow) = read_private_cli_flow(&headers, &session_key) else {
        return static_error_page(MISSING_FLOW_COOKIE);
    };
    let Some(redirect_uri) = canonical_loopback_redirect_uri(&flow.redirect_uri) else {
        let mut jar = CookieJar::new();
        remove_cli_flow_cookie(
            &mut jar,
            &session_key,
            session_cookie_secure(state.as_ref()),
        );
        let mut response = static_error_page(INVALID_REDIRECT_URI);
        append_jar_delta(response.headers_mut(), &jar);
        return response;
    };
    let secure = session_cookie_secure(state.as_ref());

    if let Some(error) = params.error.as_deref() {
        let (mapped_error, description) = match error {
            "unauthorized" => ("unauthorized", "Login not permitted"),
            "access_denied" => ("access_denied", "Authorization denied"),
            _ => ("server_error", "Could not complete GitHub sign-in"),
        };
        let mut jar = CookieJar::new();
        remove_cli_flow_cookie(&mut jar, &session_key, secure);
        let mut response =
            redirect_with_error(&redirect_uri, &flow.state, mapped_error, description);
        append_jar_delta(response.headers_mut(), &jar);
        return response;
    }

    let session = read_private_session(&headers, &session_key);
    let Some(session) = eligible_session(session.as_ref()) else {
        let mut jar = CookieJar::new();
        remove_cli_flow_cookie(&mut jar, &session_key, secure);
        let mut response = redirect_with_error(
            &redirect_uri,
            &flow.state,
            "github_session_required",
            "GitHub session required",
        );
        append_jar_delta(response.headers_mut(), &jar);
        return response;
    };

    cli_login_confirmation_page(session)
}

async fn confirm_resume(
    State(state): State<Arc<AppState>>,
    Extension(auth_mode): Extension<AuthMode>,
    headers: HeaderMap,
) -> Response {
    if !github_enabled(&auth_mode) {
        return static_error_page(GITHUB_NOT_CONFIGURED);
    }

    if !confirm_resume_origin_is_valid(&headers, state.as_ref()) {
        return static_error_page(INVALID_CONFIRMATION_REQUEST);
    }

    let Some(session_key) = state.session_key() else {
        return static_error_page(GITHUB_NOT_CONFIGURED);
    };
    let Some(flow) = read_private_cli_flow(&headers, &session_key) else {
        return static_error_page(MISSING_FLOW_COOKIE);
    };
    let Some(redirect_uri) = canonical_loopback_redirect_uri(&flow.redirect_uri) else {
        let mut jar = CookieJar::new();
        remove_cli_flow_cookie(
            &mut jar,
            &session_key,
            session_cookie_secure(state.as_ref()),
        );
        let mut response = static_error_page(INVALID_REDIRECT_URI);
        append_jar_delta(response.headers_mut(), &jar);
        return response;
    };
    let secure = session_cookie_secure(state.as_ref());

    let session = read_private_session(&headers, &session_key);
    let Some(session) = eligible_session(session.as_ref()) else {
        let mut jar = CookieJar::new();
        remove_cli_flow_cookie(&mut jar, &session_key, secure);
        let mut response = redirect_with_error(
            &redirect_uri,
            &flow.state,
            "github_session_required",
            "GitHub session required",
        );
        append_jar_delta(response.headers_mut(), &jar);
        return response;
    };

    let mut response = issue_auth_code_response(
        state.as_ref(),
        session,
        &redirect_uri,
        &flow.state,
        &flow.code_challenge,
    )
    .await;
    let mut jar = CookieJar::new();
    remove_cli_flow_cookie(&mut jar, &session_key, secure);
    append_jar_delta(response.headers_mut(), &jar);
    response
}

async fn token(
    State(state): State<Arc<AppState>>,
    Extension(auth_mode): Extension<AuthMode>,
    headers: HeaderMap,
    body: Result<Json<CliTokenRequest>, JsonRejection>,
) -> Response {
    let Some(config) = github_config(&auth_mode) else {
        return github_auth_not_configured();
    };
    let Ok(Json(body)) = body else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Invalid request",
        );
    };
    let Some(code) = body.code.as_deref() else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Invalid request",
        );
    };
    let Some(code_verifier) = body.code_verifier.as_deref() else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Invalid request",
        );
    };
    let Some(redirect_uri) = body
        .redirect_uri
        .as_deref()
        .and_then(canonical_loopback_redirect_uri)
    else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Invalid request",
        );
    };
    if body.grant_type.as_deref() != Some("authorization_code") {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Invalid request",
        );
    }

    let auth_codes = match state.store_ref().auth_codes().await {
        Ok(store) => store,
        Err(err) => {
            warn!(error = %err, "Failed to open auth code store");
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "Could not complete authentication",
            );
        }
    };
    let Some(entry) = (match auth_codes.consume(code).await {
        Ok(entry) => entry,
        Err(err) => {
            warn!(error = %err, "Failed to consume auth code");
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "Could not complete authentication",
            );
        }
    }) else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_code",
            "Invalid authorization code",
        );
    };

    if pkce_challenge(code_verifier) != entry.code_challenge {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "pkce_verification_failed",
            "PKCE verification failed",
        );
    }
    if canonical_loopback_redirect_uri(&entry.redirect_uri).as_deref()
        != Some(redirect_uri.as_str())
    {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "redirect_uri_mismatch",
            "Redirect URI mismatch",
        );
    }
    if !login_allowed(state.as_ref(), &entry.login) {
        return oauth_error(StatusCode::FORBIDDEN, "unauthorized", "Login not permitted");
    }

    let Some(jwt_key) = config.jwt_key.as_ref() else {
        return oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "Could not complete authentication",
        );
    };
    let Some(jwt_issuer) = config.jwt_issuer.as_deref() else {
        return oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "Could not complete authentication",
        );
    };

    let now = chrono::Utc::now();
    let access_expires_at = now + chrono::Duration::minutes(ACCESS_TOKEN_TTL_MINUTES);
    let refresh_expires_at = now + chrono::Duration::days(REFRESH_TOKEN_TTL_DAYS);
    let refresh_secret = random_secret();
    let refresh_token = format!("{REFRESH_TOKEN_PREFIX}{refresh_secret}");
    let refresh_row = RefreshToken {
        token_hash:   hash_refresh_secret(&refresh_secret),
        chain_id:     uuid::Uuid::new_v4(),
        identity:     entry.identity.clone(),
        login:        entry.login.clone(),
        name:         entry.name.clone(),
        email:        entry.email.clone(),
        issued_at:    now,
        expires_at:   refresh_expires_at,
        last_used_at: now,
        used:         false,
        user_agent:   sanitize_user_agent(request_user_agent(&headers)),
    };
    let auth_tokens = match state.store_ref().refresh_tokens().await {
        Ok(store) => store,
        Err(err) => {
            warn!(error = %err, "Failed to open refresh token store");
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "Could not complete authentication",
            );
        }
    };
    if let Err(err) = auth_tokens.insert_refresh_token(refresh_row.clone()).await {
        warn!(error = %err, "Failed to persist refresh token");
        return oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "Could not complete authentication",
        );
    }

    let access_token = auth::issue(
        jwt_key,
        jwt_issuer,
        &JwtSubject {
            identity:    entry.identity,
            login:       entry.login.clone(),
            name:        entry.name.clone(),
            email:       entry.email.clone(),
            avatar_url:  String::new(),
            user_url:    String::new(),
            auth_method: RunAuthMethod::Github,
        },
        chrono::Duration::minutes(ACCESS_TOKEN_TTL_MINUTES),
    );

    log_cli_auth_tokens_issued(&entry.login, &entry.email);

    Json(CliTokenResponse {
        access_token,
        access_token_expires_at: access_expires_at,
        refresh_token,
        refresh_token_expires_at: refresh_expires_at,
        subject: subject_response(
            &refresh_row.identity,
            &refresh_row.login,
            &refresh_row.name,
            &refresh_row.email,
        ),
    })
    .into_response()
}

async fn refresh(
    State(state): State<Arc<AppState>>,
    Extension(auth_mode): Extension<AuthMode>,
    headers: HeaderMap,
) -> Response {
    let Some(config) = github_config(&auth_mode) else {
        return github_auth_not_configured();
    };
    let Some(secret) = refresh_secret_from_headers(&headers) else {
        return oauth_error(
            StatusCode::UNAUTHORIZED,
            "refresh_token_expired",
            "Refresh token expired",
        );
    };
    let Some(jwt_key) = config.jwt_key.as_ref() else {
        return oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "Could not refresh authentication",
        );
    };
    let Some(jwt_issuer) = config.jwt_issuer.as_deref() else {
        return oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "Could not refresh authentication",
        );
    };
    let auth_tokens = match state.store_ref().refresh_tokens().await {
        Ok(store) => store,
        Err(err) => {
            warn!(error = %err, "Failed to open refresh token store");
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "Could not refresh authentication",
            );
        }
    };

    let now = chrono::Utc::now();
    let secret_hash = hash_refresh_secret(&secret);
    let existing = match auth_tokens.find_refresh_token(&secret_hash).await {
        Ok(existing) => existing,
        Err(err) => {
            warn!(error = %err, "Failed to load refresh token before rotation");
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "Could not refresh authentication",
            );
        }
    };
    let next_secret = random_secret();
    let next_user_agent = sanitize_user_agent(request_user_agent(&headers));
    let outcome = match auth_tokens
        .consume_and_rotate(
            secret_hash,
            next_refresh_row(existing.as_ref(), &next_secret, &next_user_agent, now),
            now,
        )
        .await
    {
        Ok(outcome) => outcome,
        Err(err) => {
            warn!(error = %err, "Failed to rotate refresh token");
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "Could not refresh authentication",
            );
        }
    };

    let (old, new_row) = match outcome {
        ConsumeOutcome::NotFound | ConsumeOutcome::Expired => {
            if auth_tokens.was_recently_replay_revoked(&secret_hash, now) {
                return oauth_error(
                    StatusCode::UNAUTHORIZED,
                    "refresh_token_revoked",
                    "Refresh token revoked",
                );
            }
            return oauth_error(
                StatusCode::UNAUTHORIZED,
                "refresh_token_expired",
                "Refresh token expired",
            );
        }
        ConsumeOutcome::Reused(old) => {
            auth_tokens.mark_refresh_token_replay(secret_hash, now);
            if let Err(err) = auth_tokens.delete_chain(old.chain_id).await {
                warn!(error = %err, chain_id = %old.chain_id, "Failed to revoke replayed refresh token chain");
            }
            log_refresh_token_replay(old.chain_id, old.identity.subject(), &next_user_agent);
            return oauth_error(
                StatusCode::UNAUTHORIZED,
                "refresh_token_revoked",
                "Refresh token revoked",
            );
        }
        ConsumeOutcome::Rotated(old, new_row) => (old, *new_row),
    };

    if !login_allowed(state.as_ref(), &old.login) {
        if let Err(err) = auth_tokens.delete_chain(old.chain_id).await {
            warn!(error = %err, chain_id = %old.chain_id, "Failed to revoke deauthorized refresh token chain");
        }
        return oauth_error(StatusCode::FORBIDDEN, "unauthorized", "Login not permitted");
    }

    let access_expires_at = now + chrono::Duration::minutes(ACCESS_TOKEN_TTL_MINUTES);
    let access_token = auth::issue(
        jwt_key,
        jwt_issuer,
        &JwtSubject {
            identity:    old.identity.clone(),
            login:       old.login.clone(),
            name:        old.name.clone(),
            email:       old.email.clone(),
            avatar_url:  String::new(),
            user_url:    String::new(),
            auth_method: RunAuthMethod::Github,
        },
        chrono::Duration::minutes(ACCESS_TOKEN_TTL_MINUTES),
    );

    Json(CliTokenResponse {
        access_token,
        access_token_expires_at: access_expires_at,
        refresh_token: format!("{REFRESH_TOKEN_PREFIX}{next_secret}"),
        refresh_token_expires_at: new_row.expires_at,
        subject: subject_response(&old.identity, &old.login, &old.name, &old.email),
    })
    .into_response()
}

async fn logout(
    State(state): State<Arc<AppState>>,
    Extension(auth_mode): Extension<AuthMode>,
    headers: HeaderMap,
) -> Response {
    if github_config(&auth_mode).is_none() {
        return github_auth_not_configured();
    }

    let Some(secret) = refresh_secret_from_headers(&headers) else {
        return StatusCode::NO_CONTENT.into_response();
    };
    let auth_tokens = match state.store_ref().refresh_tokens().await {
        Ok(store) => store,
        Err(err) => {
            warn!(error = %err, "Failed to open refresh token store");
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "Could not complete logout",
            );
        }
    };

    let existing = match auth_tokens
        .find_refresh_token(&hash_refresh_secret(&secret))
        .await
    {
        Ok(existing) => existing,
        Err(err) => {
            warn!(error = %err, "Failed to look up refresh token during logout");
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "Could not complete logout",
            );
        }
    };

    if let Some(refresh_token) = existing {
        if let Err(err) = auth_tokens.delete_chain(refresh_token.chain_id).await {
            warn!(error = %err, chain_id = %refresh_token.chain_id, "Failed to revoke refresh token chain during logout");
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "Could not complete logout",
            );
        }
        log_cli_refresh_chain_logged_out(&refresh_token.login, &refresh_token.email);
    }

    StatusCode::NO_CONTENT.into_response()
}

fn github_enabled(auth_mode: &AuthMode) -> bool {
    matches!(
        auth_mode,
        AuthMode::Enabled(config) if config.methods.contains(&ServerAuthMethod::Github)
    )
}

fn github_config(auth_mode: &AuthMode) -> Option<&ConfiguredAuth> {
    match auth_mode {
        AuthMode::Enabled(config) if config.methods.contains(&ServerAuthMethod::Github) => {
            Some(config)
        }
        _ => None,
    }
}

fn github_auth_not_configured() -> Response {
    oauth_error(
        StatusCode::FORBIDDEN,
        "github_auth_not_configured",
        "GitHub login is not configured for this server",
    )
}

fn resolved_web_url(state: &AppState) -> Option<String> {
    state.canonical_origin().ok()
}

fn session_cookie_secure(state: &AppState) -> bool {
    resolved_web_url(state).is_some_and(|web_url| web_url.starts_with("https://"))
}

fn eligible_session(session: Option<&SessionCookie>) -> Option<&SessionCookie> {
    session.filter(|session| {
        session.identity.is_some() && session.auth_method == RunAuthMethod::Github
    })
}

fn valid_state_token(state: &str) -> bool {
    (16..=512).contains(&state.len())
        && state
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn confirm_resume_origin_is_valid(headers: &HeaderMap, state: &AppState) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Some(web_url) = resolved_web_url(state) else {
        return false;
    };
    let Ok(origin_url) = Url::parse(origin) else {
        return false;
    };
    let Ok(web_url) = Url::parse(&web_url) else {
        return false;
    };

    origin_url.scheme() == web_url.scheme()
        && origin_url.host_str() == web_url.host_str()
        && origin_url.port_or_known_default() == web_url.port_or_known_default()
}

fn canonical_loopback_redirect_uri(redirect_uri: &str) -> Option<String> {
    let url = Url::parse(redirect_uri).ok()?;
    if url.scheme() != "http" {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    match url.host()? {
        Host::Ipv4(addr) if addr == net::Ipv4Addr::LOCALHOST => {}
        Host::Ipv6(addr) if addr == net::Ipv6Addr::LOCALHOST => {}
        _ => return None,
    }
    url.port()?;
    if url.path() != "/callback" || url.query().is_some() || url.fragment().is_some() {
        return None;
    }

    Some(url.to_string())
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn browser_shell(status: StatusCode, title: &str, body: &str) -> Response {
    let html = format!(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>{title} · Fabro</title>
    <link rel="icon" href="/favicon.svg" type="image/svg+xml">
    <style>
      :root {{
        color-scheme: dark;
        --page:       #0F1729;
        --overlay:    rgba(255, 255, 255, 0.04);
        --overlay-2:  rgba(255, 255, 255, 0.08);
        --line:       rgba(255, 255, 255, 0.08);
        --fg:         #ffffff;
        --fg-2:       #E8EDF3;
        --fg-3:       #A8B5C5;
        --teal-500:   #67B2D7;
        --teal-700:   #357F9E;
        --mint:       #5AC8A8;
        --coral:      #E86B6B;
        --on-primary: #0F1729;
        font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif, "Apple Color Emoji", "Segoe UI Emoji";
      }}
      * {{ box-sizing: border-box; }}
      body {{
        margin: 0;
        min-height: 100dvh;
        color: var(--fg-2);
        background-color: var(--page);
        background-image:
          radial-gradient(ellipse 120% 60% at 15% 5%, rgba(53, 127, 158, 0.14) 0%, transparent 50%),
          radial-gradient(ellipse 80% 50% at 85% 90%, rgba(90, 200, 168, 0.08) 0%, transparent 45%);
        background-attachment: fixed;
        display: flex;
        align-items: center;
        justify-content: center;
        padding: 3rem 1rem;
      }}
      main {{ width: 100%; max-width: 24rem; }}
      .brand {{
        display: flex;
        justify-content: center;
        margin-bottom: 1.75rem;
      }}
      .brand img {{ width: 3rem; height: 3rem; }}
      .panel {{
        background: rgba(37, 44, 61, 0.82);
        border: 1px solid var(--line);
        border-radius: 0.75rem;
        padding: 2rem;
        box-shadow: 0 20px 48px rgba(0, 0, 0, 0.35);
        backdrop-filter: blur(4px);
        -webkit-backdrop-filter: blur(4px);
      }}
      .stack > * + * {{ margin-top: 1.5rem; }}
      .eyebrow {{
        display: inline-flex;
        align-items: center;
        gap: 0.5rem;
        margin: 0;
        color: var(--mint);
        font-size: 0.75rem;
        font-weight: 600;
        letter-spacing: 0.06em;
        text-transform: uppercase;
      }}
      .eyebrow::before {{
        content: "";
        width: 0.375rem;
        height: 0.375rem;
        border-radius: 9999px;
        background: var(--mint);
        box-shadow: 0 0 0 3px rgba(90, 200, 168, 0.22);
      }}
      .eyebrow.error {{ color: var(--coral); }}
      .eyebrow.error::before {{
        background: var(--coral);
        box-shadow: 0 0 0 3px rgba(232, 107, 107, 0.22);
      }}
      h1 {{
        margin: 0.625rem 0 0;
        color: var(--fg);
        font-size: 1.5rem;
        line-height: 1.2;
        font-weight: 600;
        letter-spacing: -0.015em;
        text-wrap: balance;
      }}
      p {{
        margin: 0;
        color: var(--fg-3);
        font-size: 0.875rem;
        line-height: 1.6;
        text-wrap: pretty;
      }}
      code {{
        font-family: ui-monospace, "JetBrains Mono", Menlo, Consolas, monospace;
        font-size: 0.8125em;
        padding: 0.1em 0.35em;
        border-radius: 0.25rem;
        background: var(--overlay-2);
        color: var(--fg-2);
        white-space: nowrap;
      }}
      .identity {{
        display: flex;
        flex-direction: column;
        gap: 0.25rem;
        padding: 0.875rem 1rem;
        border-radius: 0.5rem;
        background: var(--overlay);
        border: 1px solid var(--line);
      }}
      .identity strong {{
        color: var(--fg);
        font-size: 0.9375rem;
        font-weight: 600;
      }}
      .identity-meta {{
        color: var(--fg-3);
        font-size: 0.8125rem;
        font-feature-settings: "tnum";
        word-break: break-word;
      }}
      .button {{
        display: inline-flex;
        align-items: center;
        justify-content: center;
        gap: 0.5rem;
        width: 100%;
        appearance: none;
        border: 0;
        border-radius: 0.5rem;
        background: var(--teal-500);
        color: var(--on-primary);
        padding: 0.625rem 1rem;
        font: inherit;
        font-size: 0.875rem;
        font-weight: 600;
        cursor: pointer;
        transition: background-color 120ms ease, color 120ms ease;
      }}
      .button:hover {{ background: var(--teal-700); color: var(--fg); }}
      .button:focus-visible {{
        outline: 2px solid var(--teal-500);
        outline-offset: 2px;
      }}
      form {{ margin: 0; }}
      @media (prefers-reduced-motion: reduce) {{
        .button {{ transition: none; }}
      }}
    </style>
  </head>
  <body>
    <main>
      <div class="brand">
        <img src="/logo.svg" alt="Fabro" width="48" height="48">
      </div>
      <div class="panel">
        <div class="stack">
          {body}
        </div>
      </div>
    </main>
  </body>
</html>"#,
    );
    let mut response = (status, Html(html)).into_response();
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

fn cli_login_confirmation_page(session: &SessionCookie) -> Response {
    let login = html_escape(&session.login);
    let display_name = if session.name.trim().is_empty() {
        &session.login
    } else {
        &session.name
    };
    let display_name = html_escape(display_name);
    let email = html_escape(&session.email);
    browser_shell(
        StatusCode::OK,
        "Authorize CLI login",
        &format!(
            r#"
<div>
  <p class="eyebrow">Signed in with GitHub</p>
  <h1>Authorize CLI login</h1>
</div>
<p>You started <code>fabro auth login</code> in a terminal on this machine. If that wasn't you, close this tab to cancel.</p>
<div class="identity">
  <strong>{display_name}</strong>
  <span class="identity-meta">@{login} · {email}</span>
</div>
<form method="post" action="/auth/cli/resume">
  <button class="button" type="submit">Continue as @{login}</button>
</form>
"#
        ),
    )
}

fn redirect_uri_with_query(redirect_uri: &str, params: &[(&str, &str)]) -> Option<String> {
    let redirect_uri = canonical_loopback_redirect_uri(redirect_uri)?;
    let mut location = String::with_capacity(redirect_uri.len() + 64);
    location.push_str(&redirect_uri);
    location.push('?');
    for (index, (key, value)) in params.iter().enumerate() {
        if index > 0 {
            location.push('&');
        }
        location.push_str(&encode_query_value(key));
        location.push('=');
        location.push_str(&encode_query_value(value));
    }
    Some(location)
}

fn encode_query_value(value: &str) -> String {
    utf8_percent_encode(value, QUERY_VALUE_ENCODE_SET).to_string()
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn login_allowed(state: &AppState, login: &str) -> bool {
    state
        .server_settings()
        .server
        .auth
        .github
        .allowed_usernames
        .iter()
        .any(|user| user == login)
}

fn request_user_agent(headers: &HeaderMap) -> &str {
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}

fn sanitize_user_agent(user_agent: &str) -> String {
    let sanitized: String = user_agent
        .chars()
        .filter(|ch| !ch.is_control())
        .take(256)
        .collect();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

fn refresh_secret_from_headers(headers: &HeaderMap) -> Option<String> {
    let bearer = headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    bearer
        .strip_prefix(REFRESH_TOKEN_PREFIX)
        .map(std::string::ToString::to_string)
}

fn hash_refresh_secret(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

fn next_refresh_row(
    existing: Option<&RefreshToken>,
    next_secret: &str,
    user_agent: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> RefreshToken {
    let fallback_identity = fabro_types::IdpIdentity::new("https://github.com", "0")
        .expect("static identity should be valid");
    RefreshToken {
        token_hash:   hash_refresh_secret(next_secret),
        chain_id:     existing.map_or_else(uuid::Uuid::new_v4, |token| token.chain_id),
        identity:     existing
            .map_or_else(|| fallback_identity.clone(), |token| token.identity.clone()),
        login:        existing.map_or_else(String::new, |token| token.login.clone()),
        name:         existing.map_or_else(String::new, |token| token.name.clone()),
        email:        existing.map_or_else(String::new, |token| token.email.clone()),
        issued_at:    now,
        expires_at:   now + chrono::Duration::days(REFRESH_TOKEN_TTL_DAYS),
        last_used_at: now,
        used:         false,
        user_agent:   user_agent.to_string(),
    }
}

fn user_agent_fingerprint(user_agent: &str) -> String {
    let digest = Sha256::digest(user_agent.as_bytes());
    hex::encode(&digest[..8])
}

fn log_cli_auth_tokens_issued(login: &str, email: &str) {
    info!(login = %login, email = %email, "Issued CLI auth tokens");
}

fn log_refresh_token_replay(chain_id: uuid::Uuid, idp_subject: &str, user_agent: &str) {
    warn!(
        chain_id = %chain_id,
        idp_subject = %idp_subject,
        user_agent_fingerprint = %user_agent_fingerprint(user_agent),
        "Refresh token replay detected"
    );
}

fn log_cli_refresh_chain_logged_out(login: &str, email: &str) {
    info!(login = %login, email = %email, "Logged out CLI refresh token chain");
}

fn oauth_error(
    status: StatusCode,
    error: &'static str,
    error_description: &'static str,
) -> Response {
    (
        status,
        Json(OAuthErrorResponse {
            error,
            error_description,
        }),
    )
        .into_response()
}

fn random_secret() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.try_fill_bytes(&mut bytes).expect("OS RNG");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn subject_response(
    identity: &fabro_types::IdpIdentity,
    login: &str,
    name: &str,
    email: &str,
) -> CliAuthSubjectResponse {
    CliAuthSubjectResponse {
        idp_issuer:  identity.issuer().to_string(),
        idp_subject: identity.subject().to_string(),
        login:       login.to_string(),
        name:        name.to_string(),
        email:       email.to_string(),
    }
}

fn redirect_with_code(redirect_uri: &str, state: &str, code: &str) -> Response {
    match redirect_uri_with_query(redirect_uri, &[("code", code), ("state", state)]) {
        Some(location) => Redirect::to(&location).into_response(),
        None => static_error_page(INVALID_REDIRECT_URI),
    }
}

fn redirect_with_error(
    redirect_uri: &str,
    state: &str,
    error: &str,
    error_description: &str,
) -> Response {
    match redirect_uri_with_query(redirect_uri, &[
        ("error", error),
        ("error_description", error_description),
        ("state", state),
    ]) {
        Some(location) => Redirect::to(&location).into_response(),
        None => static_error_page(INVALID_REDIRECT_URI),
    }
}

fn static_error_page(body: &'static str) -> Response {
    browser_shell(
        StatusCode::BAD_REQUEST,
        "Login failed",
        &format!(
            r#"
<div>
  <p class="eyebrow error">Login failed</p>
  <h1>CLI sign-in could not continue</h1>
</div>
<p>{body}</p>
<p>Return to your terminal and run <code>fabro auth login</code> again.</p>
"#
        ),
    )
}

fn append_jar_delta(headers: &mut HeaderMap, jar: &CookieJar) {
    for cookie in jar.delta() {
        if let Ok(value) = HeaderValue::from_str(&cookie.encoded().to_string()) {
            headers.append(header::SET_COOKIE, value);
        }
    }
}

fn add_cli_flow_cookie(jar: &mut CookieJar, key: &Key, flow: &CliFlowCookie, secure: bool) {
    jar.private_mut(key).add(
        Cookie::build((
            CLI_FLOW_COOKIE_NAME,
            serde_json::to_string(&flow).unwrap_or_default(),
        ))
        .path("/auth")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(secure)
        .max_age(Duration::minutes(10))
        .build(),
    );
}

fn read_private_cli_flow(headers: &HeaderMap, key: &Key) -> Option<CliFlowCookie> {
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
    jar.private(key)
        .get(CLI_FLOW_COOKIE_NAME)
        .and_then(|cookie| serde_json::from_str(cookie.value()).ok())
}

fn remove_cli_flow_cookie(jar: &mut CookieJar, key: &Key, secure: bool) {
    jar.private_mut(key).remove(
        Cookie::build((CLI_FLOW_COOKIE_NAME, ""))
            .path("/auth")
            .http_only(true)
            .secure(secure)
            .build(),
    );
}

fn random_auth_code() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.try_fill_bytes(&mut bytes).expect("OS RNG");
    URL_SAFE_NO_PAD.encode(bytes)
}

async fn issue_auth_code_response(
    state: &AppState,
    session: &SessionCookie,
    redirect_uri: &str,
    state_token: &str,
    code_challenge: &str,
) -> Response {
    let code = random_auth_code();
    let Some(identity) = session.identity.clone() else {
        return redirect_with_error(
            redirect_uri,
            state_token,
            "github_session_required",
            "GitHub session required",
        );
    };
    let Some(redirect_uri) = canonical_loopback_redirect_uri(redirect_uri) else {
        return static_error_page(INVALID_REDIRECT_URI);
    };
    let entry = AuthCode {
        code: code.clone(),
        identity,
        login: session.login.clone(),
        name: session.name.clone(),
        email: session.email.clone(),
        code_challenge: code_challenge.to_string(),
        redirect_uri: redirect_uri.clone(),
        expires_at: chrono::Utc::now() + chrono::Duration::seconds(60),
    };

    let store = match state.store_ref().auth_codes().await {
        Ok(store) => store,
        Err(err) => {
            warn!(error = %err, "Failed to open auth code store");
            return redirect_with_error(
                &redirect_uri,
                state_token,
                "server_error",
                "Could not complete GitHub sign-in",
            );
        }
    };

    if let Err(err) = store.insert(entry).await {
        warn!(error = %err, "Failed to persist auth code");
        return redirect_with_error(
            &redirect_uri,
            state_token,
            "server_error",
            "Could not complete GitHub sign-in",
        );
    }

    redirect_with_code(&redirect_uri, state_token, &code)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::sync::{Arc, Mutex as StdMutex};

    use axum::Extension;
    use axum::body::{Body, to_bytes};
    use axum::http::{HeaderMap, Request, StatusCode, header};
    use axum_extra::extract::cookie::Key;
    use fabro_config::{RunLayer, ServerSettingsBuilder};
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use fabro_types::RunAuthMethod;
    use fabro_types::settings::server::ServerAuthMethod;
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use tokio::sync::Barrier;
    use tokio::task::JoinSet;
    use tower::ServiceExt;
    use tracing::field::{Field, Visit};
    use tracing::{Event, Subscriber};
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{Layer, Registry};
    use uuid::Uuid;

    use super::{
        CliFlowCookie, add_cli_flow_cookie, read_private_cli_flow, user_agent_fingerprint,
        web_routes,
    };
    use crate::auth::{self, AuthCode, RefreshToken};
    use crate::jwt_auth::{AuthMode, ConfiguredAuth};
    use crate::server;
    use crate::web_auth::SessionCookie;

    fn test_cookie_key() -> Key {
        auth::derive_cookie_key(b"cli-flow-test-key-material-0123456789")
            .expect("test key should derive")
    }

    fn github_auth_mode() -> AuthMode {
        let mut config = ConfiguredAuth::new(vec![ServerAuthMethod::Github], None);
        config.jwt_key = Some(
            auth::derive_jwt_key(b"cli-flow-test-key-material-0123456789")
                .expect("test key should derive"),
        );
        config.jwt_issuer = Some("https://fabro.example".to_string());
        AuthMode::Enabled(config)
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

    fn test_router(
        settings: fabro_types::ServerSettings,
    ) -> (axum::Router, Arc<crate::server::AppState>) {
        let state = server::create_test_app_state_with_runtime_settings_and_session_key(
            settings,
            RunLayer::default(),
            Some("cli-flow-test-key-material-0123456789"),
        );
        let app = axum::Router::new()
            .nest("/auth", web_routes())
            .layer(Extension(github_auth_mode()))
            .with_state(Arc::clone(&state));
        (app, state)
    }

    fn github_session_cookie(key: &Key) -> String {
        let session = SessionCookie {
            v:           2,
            login:       "octocat".to_string(),
            auth_method: RunAuthMethod::Github,
            identity:    Some(
                fabro_types::IdpIdentity::new("https://github.com", "12345")
                    .expect("identity should be valid"),
            ),
            name:        "The Octocat".to_string(),
            email:       "octocat@example.com".to_string(),
            avatar_url:  "https://example.com/octocat.png".to_string(),
            user_url:    "https://github.com/octocat".to_string(),
            iat:         chrono::Utc::now().timestamp(),
            exp:         (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp(),
        };
        let mut jar = cookie::CookieJar::new();
        jar.private_mut(key).add(cookie::Cookie::new(
            crate::web_auth::SESSION_COOKIE_NAME,
            serde_json::to_string(&session).unwrap(),
        ));
        jar.delta()
            .next()
            .expect("session cookie should exist")
            .encoded()
            .to_string()
    }

    fn pkce_challenge(verifier: &str) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    }

    async fn insert_auth_code(state: &crate::server::AppState, code: &str, verifier: &str) {
        let auth_codes = state.store_ref().auth_codes().await.unwrap();
        auth_codes
            .insert(AuthCode {
                code:           code.to_string(),
                identity:       fabro_types::IdpIdentity::new("https://github.com", "12345")
                    .expect("identity should be valid"),
                login:          "octocat".to_string(),
                name:           "The Octocat".to_string(),
                email:          "octocat@example.com".to_string(),
                code_challenge: pkce_challenge(verifier),
                redirect_uri:   "http://127.0.0.1:4444/callback".to_string(),
                expires_at:     chrono::Utc::now() + chrono::Duration::seconds(60),
            })
            .await
            .unwrap();
    }

    fn hash_refresh_secret(secret: &str) -> [u8; 32] {
        Sha256::digest(secret.as_bytes()).into()
    }

    fn refresh_row(secret: &str) -> RefreshToken {
        let now = chrono::Utc::now();
        RefreshToken {
            token_hash:   hash_refresh_secret(secret),
            chain_id:     Uuid::new_v4(),
            identity:     fabro_types::IdpIdentity::new("https://github.com", "12345")
                .expect("identity should be valid"),
            login:        "octocat".to_string(),
            name:         "The Octocat".to_string(),
            email:        "octocat@example.com".to_string(),
            issued_at:    now,
            expires_at:   now + chrono::Duration::days(30),
            last_used_at: now,
            used:         false,
            user_agent:   "fabro-test".to_string(),
        }
    }

    #[derive(Default)]
    struct EventCapture {
        fields: Vec<(String, String)>,
    }

    impl Visit for EventCapture {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.fields
                .push((field.name().to_string(), format!("{value:?}")));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }

        fn record_i64(&mut self, field: &Field, value: i64) {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }

        fn record_bool(&mut self, field: &Field, value: bool) {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }
    }

    struct CaptureLayer {
        lines: Arc<StdMutex<Vec<String>>>,
    }

    impl<S: Subscriber> Layer<S> for CaptureLayer {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let mut capture = EventCapture::default();
            event.record(&mut capture);

            let mut line = event.metadata().level().to_string();
            for (field, value) in capture.fields {
                let _ = write!(line, " {field}={value}");
            }
            self.lines.lock().unwrap().push(line);
        }
    }

    fn capture_cli_flow_events(run: impl FnOnce()) -> Arc<StdMutex<Vec<String>>> {
        let lines = Arc::new(StdMutex::new(Vec::new()));
        let subscriber = Registry::default().with(CaptureLayer {
            lines: Arc::clone(&lines),
        });
        tracing::subscriber::with_default(subscriber, run);
        lines
    }

    #[tokio::test]
    async fn start_without_session_sets_flow_cookie_and_redirects_to_github_login() {
        let key = test_cookie_key();
        let (app, _state) = test_router(github_settings("https://fabro.example"));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/auth/cli/start?redirect_uri=http://127.0.0.1:4444/callback&state=abcdefghijklmnop&code_challenge=challenge&code_challenge_method=S256")
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
                .and_then(|value| value.to_str().ok()),
            Some("/auth/login/github?return_to=/auth/cli/resume")
        );

        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .expect("flow cookie should be set")
            .to_string();
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, cookie.parse().unwrap());
        let flow = read_private_cli_flow(&headers, &key).expect("flow cookie should decode");
        assert_eq!(flow, CliFlowCookie {
            redirect_uri:   "http://127.0.0.1:4444/callback".to_string(),
            state:          "abcdefghijklmnop".to_string(),
            code_challenge: "challenge".to_string(),
        });
    }

    #[tokio::test]
    async fn start_with_github_session_sets_flow_cookie_and_redirects_to_resume() {
        let key = test_cookie_key();
        let (app, _state) = test_router(github_settings("https://fabro.example"));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/auth/cli/start?redirect_uri=http://127.0.0.1:4444/callback&state=abcdefghijklmnop&code_challenge=challenge&code_challenge_method=S256")
                    .header(header::COOKIE, github_session_cookie(&key))
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
                .and_then(|value| value.to_str().ok()),
            Some("/auth/cli/resume")
        );

        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .expect("flow cookie should be set")
            .to_string();
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, cookie.parse().unwrap());
        let flow = read_private_cli_flow(&headers, &key).expect("flow cookie should decode");
        assert_eq!(flow, CliFlowCookie {
            redirect_uri:   "http://127.0.0.1:4444/callback".to_string(),
            state:          "abcdefghijklmnop".to_string(),
            code_challenge: "challenge".to_string(),
        });
    }

    #[tokio::test]
    async fn resume_with_github_session_renders_confirmation_page() {
        let key = test_cookie_key();
        let (app, _state) = test_router(github_settings("https://fabro.example"));
        let mut jar = cookie::CookieJar::new();
        add_cli_flow_cookie(
            &mut jar,
            &key,
            &CliFlowCookie {
                redirect_uri:   "http://127.0.0.1:4444/callback".to_string(),
                state:          "abcdefghijklmnop".to_string(),
                code_challenge: "challenge".to_string(),
            },
            true,
        );
        let flow_cookie = jar
            .delta()
            .next()
            .expect("flow cookie should exist")
            .encoded()
            .to_string();
        let cookie_header = format!("{}; {}", github_session_cookie(&key), flow_cookie);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/auth/cli/resume")
                    .header(header::COOKIE, cookie_header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("Authorize CLI login"));
        assert!(html.contains("Continue as @octocat"));
        assert!(html.contains("form"));
        assert!(html.contains("method=\"post\""));
    }

    #[tokio::test]
    async fn post_resume_with_github_session_mints_auth_code_and_redirects_to_loopback() {
        let key = test_cookie_key();
        let (app, state) = test_router(github_settings("https://fabro.example"));
        let mut jar = cookie::CookieJar::new();
        add_cli_flow_cookie(
            &mut jar,
            &key,
            &CliFlowCookie {
                redirect_uri:   "http://127.0.0.1:4444/callback".to_string(),
                state:          "abcdefghijklmnop".to_string(),
                code_challenge: "challenge".to_string(),
            },
            true,
        );
        let flow_cookie = jar
            .delta()
            .next()
            .expect("flow cookie should exist")
            .encoded()
            .to_string();
        let cookie_header = format!("{}; {}", github_session_cookie(&key), flow_cookie);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/cli/resume")
                    .header(header::COOKIE, cookie_header)
                    .header(header::ORIGIN, "https://fabro.example")
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
            .expect("loopback redirect should be set");
        assert!(location.starts_with("http://127.0.0.1:4444/callback?code="));
        assert!(location.ends_with("&state=abcdefghijklmnop"));

        let code = location
            .split("code=")
            .nth(1)
            .and_then(|segment| segment.split('&').next())
            .expect("auth code should be present");
        let auth_codes = state.store_ref().auth_codes().await.unwrap();
        let entry = auth_codes
            .consume(code)
            .await
            .unwrap()
            .expect("code should exist");
        assert_eq!(entry.redirect_uri, "http://127.0.0.1:4444/callback");
        assert_eq!(entry.code_challenge, "challenge");
        assert_eq!(entry.login, "octocat");
    }

    #[tokio::test]
    async fn post_resume_rejects_origin_mismatch_with_html_error() {
        let key = test_cookie_key();
        let (app, _state) = test_router(github_settings("https://fabro.example"));
        let mut jar = cookie::CookieJar::new();
        add_cli_flow_cookie(
            &mut jar,
            &key,
            &CliFlowCookie {
                redirect_uri:   "http://127.0.0.1:4444/callback".to_string(),
                state:          "abcdefghijklmnop".to_string(),
                code_challenge: "challenge".to_string(),
            },
            true,
        );
        let flow_cookie = jar
            .delta()
            .next()
            .expect("flow cookie should exist")
            .encoded()
            .to_string();
        let cookie_header = format!("{}; {}", github_session_cookie(&key), flow_cookie);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/cli/resume")
                    .header(header::COOKIE, cookie_header)
                    .header(header::ORIGIN, "https://attacker.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("Login failed"));
    }

    #[tokio::test]
    async fn post_resume_without_github_session_redirects_with_github_session_required() {
        let key = test_cookie_key();
        let (app, _state) = test_router(github_settings("https://fabro.example"));
        let mut jar = cookie::CookieJar::new();
        add_cli_flow_cookie(
            &mut jar,
            &key,
            &CliFlowCookie {
                redirect_uri:   "http://127.0.0.1:4444/callback".to_string(),
                state:          "abcdefghijklmnop".to_string(),
                code_challenge: "challenge".to_string(),
            },
            true,
        );
        let flow_cookie = jar
            .delta()
            .next()
            .expect("flow cookie should exist")
            .encoded()
            .to_string();

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/cli/resume")
                    .header(header::COOKIE, flow_cookie)
                    .header(header::ORIGIN, "https://fabro.example")
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
                .and_then(|value| value.to_str().ok()),
            Some(
                "http://127.0.0.1:4444/callback?error=github_session_required&error_description=GitHub%20session%20required&state=abcdefghijklmnop"
            )
        );
    }

    #[tokio::test]
    async fn resume_forwards_error_without_checking_session() {
        let key = test_cookie_key();
        let (app, _state) = test_router(github_settings("https://fabro.example"));
        let mut jar = cookie::CookieJar::new();
        add_cli_flow_cookie(
            &mut jar,
            &key,
            &CliFlowCookie {
                redirect_uri:   "http://127.0.0.1:4444/callback".to_string(),
                state:          "abcdefghijklmnop".to_string(),
                code_challenge: "challenge".to_string(),
            },
            true,
        );
        let cookie = jar
            .delta()
            .next()
            .expect("flow cookie should exist")
            .encoded()
            .to_string();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/auth/cli/resume?error=access_denied")
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
                .and_then(|value| value.to_str().ok()),
            Some(
                "http://127.0.0.1:4444/callback?error=access_denied&error_description=Authorization%20denied&state=abcdefghijklmnop"
            )
        );
    }

    #[tokio::test]
    async fn start_rejects_invalid_redirect_uri_with_html_error() {
        let (app, _state) = test_router(github_settings("https://fabro.example"));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/auth/cli/start?redirect_uri=http://localhost:4444/callback&state=abcdefghijklmnop&code_challenge=challenge&code_challenge_method=S256")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response
                .headers()
                .get("x-content-type-options")
                .and_then(|value| value.to_str().ok()),
            Some("nosniff")
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("The provided redirect URI is not valid for CLI login."));
    }

    #[tokio::test]
    async fn start_rejects_userinfo_injected_redirect_uri_with_html_error() {
        let (app, _state) = test_router(github_settings("https://fabro.example"));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/auth/cli/start?redirect_uri=http://127.0.0.1:1@attacker.com/callback&state=abcdefghijklmnop&code_challenge=challenge&code_challenge_method=S256")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("The provided redirect URI is not valid for CLI login."));
    }

    #[tokio::test]
    async fn token_exchanges_code_for_access_and_refresh_tokens() {
        let (app, state) = test_router(github_settings("https://fabro.example"));
        insert_auth_code(state.as_ref(), "auth-code-1", "test-verifier").await;

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/cli/token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::USER_AGENT, "fabro-cli/0.1")
                    .body(Body::from(
                        json!({
                            "grant_type": "authorization_code",
                            "code": "auth-code-1",
                            "code_verifier": "test-verifier",
                            "redirect_uri": "http://127.0.0.1:4444/callback"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert!(body["access_token"].as_str().unwrap().starts_with("eyJ"));
        assert!(
            body["refresh_token"]
                .as_str()
                .unwrap()
                .starts_with("fabro_refresh_")
        );
        assert_eq!(body["subject"]["idp_issuer"], "https://github.com");
        assert_eq!(body["subject"]["idp_subject"], "12345");
        assert_eq!(body["subject"]["login"], "octocat");

        let refresh_secret = body["refresh_token"]
            .as_str()
            .unwrap()
            .strip_prefix("fabro_refresh_")
            .unwrap();
        let auth_tokens = state.store_ref().refresh_tokens().await.unwrap();
        let refresh = auth_tokens
            .find_refresh_token(&hash_refresh_secret(refresh_secret))
            .await
            .unwrap()
            .expect("refresh token should be stored");
        assert_eq!(refresh.login, "octocat");
        assert_eq!(refresh.user_agent, "fabro-cli/0.1");
    }

    #[tokio::test]
    async fn token_wrong_verifier_fails_and_burns_code() {
        let (app, state) = test_router(github_settings("https://fabro.example"));
        insert_auth_code(state.as_ref(), "auth-code-2", "correct-verifier").await;

        let wrong_verifier = || {
            Request::builder()
                .method("POST")
                .uri("/auth/cli/token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "grant_type": "authorization_code",
                        "code": "auth-code-2",
                        "code_verifier": "wrong-verifier",
                        "redirect_uri": "http://127.0.0.1:4444/callback"
                    })
                    .to_string(),
                ))
                .unwrap()
        };

        let first = app.clone().oneshot(wrong_verifier()).await.unwrap();
        assert_eq!(first.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(first.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["error"], "pkce_verification_failed");

        let second = app.oneshot(wrong_verifier()).await.unwrap();
        assert_eq!(second.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(second.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["error"], "invalid_code");
    }

    #[tokio::test]
    async fn token_rejects_userinfo_injected_redirect_uri() {
        let (app, state) = test_router(github_settings("https://fabro.example"));
        insert_auth_code(
            state.as_ref(),
            "auth-code-malicious-redirect",
            "test-verifier",
        )
        .await;

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/cli/token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "grant_type": "authorization_code",
                            "code": "auth-code-malicious-redirect",
                            "code_verifier": "test-verifier",
                            "redirect_uri": "http://127.0.0.1:1@attacker.com/callback"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["error"], "invalid_request");
        assert_eq!(body["error_description"], "Invalid request");
    }

    #[tokio::test]
    async fn refresh_rotates_tokens_and_replay_revokes_chain() {
        let (app, state) = test_router(github_settings("https://fabro.example"));
        let initial_secret = "refresh-secret-1";
        let auth_tokens = state.store_ref().refresh_tokens().await.unwrap();
        auth_tokens
            .insert_refresh_token(refresh_row(initial_secret))
            .await
            .unwrap();

        let refresh_request = || {
            Request::builder()
                .method("POST")
                .uri("/auth/cli/refresh")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer fabro_refresh_{initial_secret}"),
                )
                .header(header::USER_AGENT, "fabro-cli/0.2")
                .body(Body::empty())
                .unwrap()
        };

        let first = app.clone().oneshot(refresh_request()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let first_body: serde_json::Value =
            serde_json::from_slice(&to_bytes(first.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        let rotated = first_body["refresh_token"].as_str().unwrap().to_string();
        assert_ne!(rotated, format!("fabro_refresh_{initial_secret}"));

        let replay = app.oneshot(refresh_request()).await.unwrap();
        assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
        let replay_body: serde_json::Value =
            serde_json::from_slice(&to_bytes(replay.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(replay_body["error"], "refresh_token_revoked");

        let new_secret = rotated.strip_prefix("fabro_refresh_").unwrap();
        assert!(
            auth_tokens
                .find_refresh_token(&hash_refresh_secret(initial_secret))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            auth_tokens
                .find_refresh_token(&hash_refresh_secret(new_secret))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_refresh_has_one_winner_and_revokes_chain() {
        let (app, state) = test_router(github_settings("https://fabro.example"));
        let initial_secret = "refresh-secret-concurrent";
        let auth_tokens = state.store_ref().refresh_tokens().await.unwrap();
        auth_tokens
            .insert_refresh_token(refresh_row(initial_secret))
            .await
            .unwrap();

        let barrier = Arc::new(Barrier::new(33));
        let mut tasks = JoinSet::new();
        for _ in 0..32 {
            let app = app.clone();
            let barrier = Arc::clone(&barrier);
            tasks.spawn(async move {
                barrier.wait().await;
                let response = app
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri("/auth/cli/refresh")
                            .header(
                                header::AUTHORIZATION,
                                format!("Bearer fabro_refresh_{initial_secret}"),
                            )
                            .header(header::USER_AGENT, "fabro-cli/0.3")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = response.status();
                let body: serde_json::Value = serde_json::from_slice(
                    &to_bytes(response.into_body(), usize::MAX).await.unwrap(),
                )
                .unwrap();
                (status, body)
            });
        }
        barrier.wait().await;

        let mut success = 0;
        let mut revoked = 0;
        let mut rotated_secret = None;
        while let Some(result) = tasks.join_next().await {
            let (status, body) = result.unwrap();
            match status {
                StatusCode::OK => {
                    success += 1;
                    rotated_secret = body["refresh_token"]
                        .as_str()
                        .and_then(|token| token.strip_prefix("fabro_refresh_"))
                        .map(str::to_string);
                }
                StatusCode::UNAUTHORIZED => {
                    assert_eq!(body["error"], "refresh_token_revoked");
                    revoked += 1;
                }
                other => panic!("unexpected refresh status {other}: {body}"),
            }
        }

        assert_eq!(success, 1);
        assert_eq!(revoked, 31);
        let rotated_secret = rotated_secret.expect("one refresh should rotate the token");
        assert!(
            auth_tokens
                .find_refresh_token(&hash_refresh_secret(initial_secret))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            auth_tokens
                .find_refresh_token(&hash_refresh_secret(&rotated_secret))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn logout_deletes_refresh_token_chain_and_returns_no_content() {
        let (app, state) = test_router(github_settings("https://fabro.example"));
        let secret = "refresh-secret-logout";
        let token = refresh_row(secret);
        let chain_id = token.chain_id;
        let auth_tokens = state.store_ref().refresh_tokens().await.unwrap();
        auth_tokens.insert_refresh_token(token).await.unwrap();

        let sibling = RefreshToken {
            token_hash: hash_refresh_secret("refresh-secret-logout-2"),
            chain_id,
            ..refresh_row("refresh-secret-logout-2")
        };
        auth_tokens.insert_refresh_token(sibling).await.unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/cli/logout")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer fabro_refresh_{secret}"),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            auth_tokens
                .find_refresh_token(&hash_refresh_secret(secret))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            auth_tokens
                .find_refresh_token(&hash_refresh_secret("refresh-secret-logout-2"))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn cli_flow_logs_do_not_leak_secrets_or_terminal_control_bytes() {
        let raw_refresh_token = "fabro_refresh_secret_forbidden";
        let raw_jwt = "eyJsecret.forbidden";
        let raw_code_verifier = "raw-code-verifier";
        let user_agent = "fabro-cli/0.4\x1b[31m\nspoofed";

        let captured = capture_cli_flow_events(|| {
            super::log_cli_auth_tokens_issued("octocat", "octocat@example.com");
            super::log_refresh_token_replay(uuid::Uuid::nil(), "12345", user_agent);
            super::log_cli_refresh_chain_logged_out("octocat", "octocat@example.com");

            let _ = (raw_refresh_token, raw_jwt, raw_code_verifier);
        });

        let lines = captured.lock().unwrap().clone();
        let rendered = lines.join(" ");
        assert!(
            rendered.contains("Issued CLI auth tokens"),
            "captured logs: {rendered}"
        );
        assert!(
            rendered.contains("Refresh token replay detected"),
            "captured logs: {rendered}"
        );
        assert!(
            rendered.contains("Logged out CLI refresh token chain"),
            "captured logs: {rendered}"
        );
        assert!(!rendered.contains("fabro_refresh_"));
        assert!(!rendered.contains("eyJ"));
        assert!(!rendered.contains("raw-code-verifier"));
        let fingerprint = user_agent_fingerprint(user_agent);
        assert!(!fingerprint.contains('\u{1b}'));
        assert!(!fingerprint.contains('\n'));
        assert!(lines.iter().all(|line| !line.contains('\u{1b}')));
        assert!(lines.iter().all(|line| !line.contains('\n')));
    }
}
