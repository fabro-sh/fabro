use std::fmt::Write as _;

use anyhow::{Context as _, anyhow, bail};
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use fabro_redact::DisplaySafeUrl;
use fabro_util::browser;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

// ---------------------------------------------------------------------------
// Browser shell for CLI callback responses
// ---------------------------------------------------------------------------

const FABRO_LOGO_SVG: &str = r##"<svg viewBox="0 0 272 348" width="48" height="48" aria-label="Fabro" xmlns="http://www.w3.org/2000/svg"><path d="M1 237 L62 272 L61 348 L0 312 Z M98 257 L132 275 L132 312 L71 347 L70 272 Z M202 168 L201 230 L141 264 L142 202 Z M70 169 L132 203 L132 264 L70 230 Z M70 241 L90 251 L70 262 Z M3 129 L63 164 L61 262 L1 227 Z M137 125 L196 160 L137 195 L78 160 Z M271 44 L272 119 L211 154 L210 79 Z M132 43 L132 118 L71 153 L70 78 Z M142 43 L202 77 L201 152 L141 118 Z M1 44 L62 78 L62 152 L1 118 Z M206 0 L266 36 L206 72 L146 36 Z M66 1 L126 36 L66 71 L6 36 Z" fill="#67b2d7"/></svg>"##;

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Renders the Fabro-branded shell used for every response the CLI's
/// ephemeral loopback server emits during `fabro auth login`. Fully
/// self-contained: no external assets, no fonts, inline SVG logo.
fn browser_shell(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>{title} · Fabro</title>
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
        --mint:       #5AC8A8;
        --coral:      #E86B6B;
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
      .brand svg {{ width: 3rem; height: 3rem; }}
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
    </style>
  </head>
  <body>
    <main>
      <div class="brand">{FABRO_LOGO_SVG}</div>
      <div class="panel">
        <div class="stack">
          {body}
        </div>
      </div>
    </main>
  </body>
</html>"#
    )
}

fn callback_success_page() -> String {
    browser_shell(
        "Signed in",
        r#"
<div>
  <p class="eyebrow">Signed in</p>
  <h1>You're signed in to Fabro</h1>
</div>
<p>You can close this tab and return to your terminal.</p>
"#,
    )
}

fn callback_error_page(detail: &str) -> String {
    let detail = html_escape(detail);
    browser_shell(
        "Sign-in failed",
        &format!(
            r#"
<div>
  <p class="eyebrow error">Sign-in failed</p>
  <h1>CLI sign-in could not continue</h1>
</div>
<p>{detail}</p>
<p>Return to your terminal and run <code>fabro auth login</code> again.</p>
"#
        ),
    )
}

// ---------------------------------------------------------------------------
// PKCE
// ---------------------------------------------------------------------------

pub struct PkceCodes {
    pub verifier:  String,
    pub challenge: String,
}

pub fn generate_pkce() -> PkceCodes {
    let bytes: [u8; 32] = rand::random();
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    PkceCodes {
        verifier,
        challenge,
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub fn generate_state() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
}

// ---------------------------------------------------------------------------
// URL encoding helpers
// ---------------------------------------------------------------------------

fn percent_encode_param(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

fn encode_form(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode_param(k), percent_encode_param(v)))
        .collect::<Vec<_>>()
        .join("&")
}

// ---------------------------------------------------------------------------
// Auth URL
// ---------------------------------------------------------------------------

pub fn build_authorize_url(
    issuer: &str,
    client_id: &str,
    redirect_uri: &str,
    scope: &str,
    pkce: &PkceCodes,
    state: &str,
) -> String {
    let params = encode_form(&[
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("scope", scope),
        ("code_challenge", &pkce.challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
    ]);
    format!("{issuer}/oauth/authorize?{params}")
}

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

pub struct OAuthEndpoint<'a> {
    pub token_url: &'a str,
    pub client_id: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub id_token:      Option<String>,
    pub access_token:  String,
    pub refresh_token: Option<String>,
    pub expires_in:    Option<u64>,
}

pub struct CallbackHandle {
    port:        u16,
    shutdown_tx: std::sync::Arc<std::sync::Mutex<Option<oneshot::Sender<()>>>>,
}

impl CallbackHandle {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn redirect_uri(&self, path: &str) -> String {
        build_redirect_uri(self.port, path)
    }

    pub fn shutdown(&self) {
        if let Some(tx) = self.shutdown_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackSuccess {
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackFailure {
    pub error_code:        String,
    pub error_description: String,
}

pub type CallbackResult = Result<CallbackSuccess, CallbackFailure>;

// ---------------------------------------------------------------------------
// Token exchange
// ---------------------------------------------------------------------------

pub async fn exchange_code(
    endpoint: OAuthEndpoint<'_>,
    code: &str,
    redirect_uri: Option<&str>,
    verifier: Option<&str>,
) -> anyhow::Result<TokenResponse> {
    let token_url = redacted_url_for_log(endpoint.token_url);
    tracing::debug!(
        token_url = %token_url,
        "Exchanging authorization code"
    );

    let mut params = vec![
        ("grant_type", "authorization_code"),
        ("client_id", endpoint.client_id),
        ("code", code),
    ];
    if let Some(redirect_uri) = redirect_uri {
        params.push(("redirect_uri", redirect_uri));
    }
    if let Some(verifier) = verifier {
        params.push(("code_verifier", verifier));
    }

    let body = encode_form(&params);
    let client = fabro_http::http_client().map_err(anyhow::Error::new)?;
    let resp = client
        .post(endpoint.token_url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .context("Token exchange request failed")?;

    let status = resp.status();
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        tracing::error!(%status, "Token exchange failed");
        bail!("Token exchange failed ({status}): {body_text}");
    }

    let tokens: TokenResponse = resp
        .json()
        .await
        .context("Failed to parse token response")?;

    tracing::info!(expires_in = ?tokens.expires_in, "Token exchange completed");
    Ok(tokens)
}

// ---------------------------------------------------------------------------
// Token refresh
// ---------------------------------------------------------------------------

pub async fn refresh_token(
    endpoint: OAuthEndpoint<'_>,
    refresh_token: &str,
) -> anyhow::Result<TokenResponse> {
    let token_url = redacted_url_for_log(endpoint.token_url);
    tracing::debug!(token_url = %token_url, "Refreshing access token");

    let body = encode_form(&[
        ("grant_type", "refresh_token"),
        ("client_id", endpoint.client_id),
        ("refresh_token", refresh_token),
    ]);

    let client = fabro_http::http_client().map_err(anyhow::Error::new)?;
    let resp = client
        .post(endpoint.token_url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .context("Token refresh request failed")?;

    let status = resp.status();
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        tracing::warn!(%status, "Token refresh failed");
        bail!("Token refresh failed ({status}): {body_text}");
    }

    let tokens: TokenResponse = resp
        .json()
        .await
        .context("Failed to parse refresh token response")?;

    tracing::info!(expires_in = ?tokens.expires_in, "Token refreshed");
    Ok(tokens)
}

// ---------------------------------------------------------------------------
// Callback server
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CallbackParams {
    code:              Option<String>,
    state:             String,
    error:             Option<String>,
    error_description: Option<String>,
}

fn validate_callback_path(path: &str) -> anyhow::Result<()> {
    if path.is_empty() {
        bail!("Callback path must not be empty");
    }
    if !path.starts_with('/') {
        bail!("Callback path must start with '/': {path}");
    }
    if path
        .split('/')
        .skip(1)
        .any(|segment| segment.starts_with(':') || segment.starts_with('*'))
    {
        bail!("Callback path must not contain route parameters: {path}");
    }
    Ok(())
}

fn redacted_url_for_log(url: &str) -> String {
    DisplaySafeUrl::parse(url)
        .map_or_else(|_| "<invalid url>".to_string(), |url| url.redacted_string())
}

fn build_redirect_uri(port: u16, path: &str) -> String {
    format!("http://127.0.0.1:{port}{path}")
}

pub async fn start_callback_server(
    port: u16,
    path: &str,
    expected_state: String,
) -> anyhow::Result<(u16, oneshot::Receiver<Result<String, String>>)> {
    validate_callback_path(path)?;

    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .context("Failed to bind callback server")?;
    let actual_port = listener
        .local_addr()
        .context("Failed to get local address")?
        .port();

    let (code_tx, code_rx) = oneshot::channel::<Result<String, String>>();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let code_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(code_tx)));
    let shutdown_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(shutdown_tx)));
    let expected_state = std::sync::Arc::new(expected_state);
    let callback_path = path.to_string();

    let app = axum::Router::new().route(
        callback_path.as_str(),
        get(
            move |Query(params): Query<CallbackParams>| async move {
                if params.state != *expected_state {
                    return (
                        StatusCode::BAD_REQUEST,
                        Html(callback_error_page(
                            "The sign-in state did not match. This usually means the browser session was reused across attempts.",
                        )),
                    );
                }

                if let Some(error) = params.error {
                    let desc = params
                        .error_description
                        .unwrap_or_else(|| error.clone());
                    if let Some(tx) = code_tx.lock().unwrap().take() {
                        let _ = tx.send(Err(desc.clone()));
                    }
                    if let Some(tx) = shutdown_tx.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                    return (
                        StatusCode::BAD_REQUEST,
                        Html(callback_error_page(&desc)),
                    );
                }

                let Some(code) = params.code else {
                    if let Some(tx) = code_tx.lock().unwrap().take() {
                        let _ = tx.send(Err("No authorization code received".to_string()));
                    }
                    if let Some(tx) = shutdown_tx.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                    return (
                        StatusCode::BAD_REQUEST,
                        Html(callback_error_page(
                            "The identity provider did not return an authorization code.",
                        )),
                    );
                };

                if let Some(tx) = code_tx.lock().unwrap().take() {
                    let _ = tx.send(Ok(code));
                }
                if let Some(tx) = shutdown_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                (StatusCode::OK, Html(callback_success_page()))
            },
        ),
    );

    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .ok();
    });

    Ok((actual_port, code_rx))
}

pub async fn start_callback_server_with_errors(
    expected_state: String,
    port: u16,
    path: &str,
) -> anyhow::Result<(CallbackHandle, oneshot::Receiver<CallbackResult>)> {
    validate_callback_path(path)?;

    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .context("Failed to bind callback server")?;
    let actual_port = listener
        .local_addr()
        .context("Failed to get local address")?
        .port();

    let (callback_tx, callback_rx) = oneshot::channel::<CallbackResult>();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let callback_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(callback_tx)));
    let shutdown_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(shutdown_tx)));
    let route_shutdown_tx = shutdown_tx.clone();
    let expected_state = std::sync::Arc::new(expected_state);
    let callback_path = path.to_string();

    let app = axum::Router::new().route(
        callback_path.as_str(),
        get(move |Query(params): Query<CallbackParams>| async move {
            if params.state != *expected_state {
                return (
                    StatusCode::BAD_REQUEST,
                    Html(callback_error_page(
                        "The sign-in state did not match. This usually means the browser session was reused across attempts.",
                    )),
                );
            }

            if let Some(error_code) = params.error {
                let error_description = params
                    .error_description
                    .unwrap_or_else(|| error_code.clone());
                if let Some(tx) = callback_tx.lock().unwrap().take() {
                    let _ = tx.send(Err(CallbackFailure {
                        error_code:        error_code.clone(),
                        error_description: error_description.clone(),
                    }));
                }
                if let Some(tx) = route_shutdown_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                return (
                    StatusCode::BAD_REQUEST,
                    Html(callback_error_page(&error_description)),
                );
            }

            let Some(code) = params.code else {
                if let Some(tx) = callback_tx.lock().unwrap().take() {
                    let _ = tx.send(Err(CallbackFailure {
                        error_code:        "invalid_request".to_string(),
                        error_description: "No authorization code received".to_string(),
                    }));
                }
                if let Some(tx) = route_shutdown_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                return (
                    StatusCode::BAD_REQUEST,
                    Html(callback_error_page(
                        "The identity provider did not return an authorization code.",
                    )),
                );
            };

            if let Some(tx) = callback_tx.lock().unwrap().take() {
                let _ = tx.send(Ok(CallbackSuccess { code }));
            }
            if let Some(tx) = route_shutdown_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            (StatusCode::OK, Html(callback_success_page()))
        }),
    );

    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .ok();
    });

    Ok((
        CallbackHandle {
            port: actual_port,
            shutdown_tx,
        },
        callback_rx,
    ))
}

// ---------------------------------------------------------------------------
// Browser flow
// ---------------------------------------------------------------------------

pub async fn run_browser_flow(
    issuer: &str,
    client_id: &str,
    scope: &str,
    port: u16,
    callback_path: &str,
) -> anyhow::Result<TokenResponse> {
    let pkce = generate_pkce();
    let state = generate_state();

    let (actual_port, code_rx) = start_callback_server(port, callback_path, state.clone()).await?;
    let redirect_uri = build_redirect_uri(actual_port, callback_path);
    let auth_url = build_authorize_url(issuer, client_id, &redirect_uri, scope, &pkce, &state);

    tracing::info!(
        port = actual_port,
        callback_path,
        "OAuth browser flow started"
    );

    if let Err(e) = browser::try_open(&auth_url) {
        tracing::warn!("Could not open browser: {e}");
    }

    let code = code_rx
        .await
        .map_err(|_| anyhow!("Did not receive authorization code"))?
        .map_err(|e| anyhow!("Authorization failed: {e}"))?;

    let token_url = format!("{issuer}/oauth/token");
    exchange_code(
        OAuthEndpoint {
            token_url: &token_url,
            client_id,
        },
        &code,
        Some(&redirect_uri),
        Some(&pkce.verifier),
    )
    .await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use fabro_test::assert_reqwest_status;

    use super::*;

    fn test_http_client() -> fabro_http::HttpClient {
        fabro_http::test_http_client().unwrap()
    }

    // -----------------------------------------------------------------------
    // Phase 1: PKCE
    // -----------------------------------------------------------------------

    #[test]
    fn pkce_verifier_is_43_chars() {
        let pkce = generate_pkce();
        assert_eq!(pkce.verifier.len(), 43);
    }

    #[test]
    fn pkce_verifier_is_base64url() {
        let pkce = generate_pkce();
        assert!(
            pkce.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "verifier contains invalid chars: {}",
            pkce.verifier
        );
    }

    #[test]
    fn pkce_challenge_is_sha256_of_verifier() {
        let pkce = generate_pkce();
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier.as_bytes()));
        assert_eq!(pkce.challenge, expected);
    }

    #[test]
    fn pkce_unique_per_call() {
        let a = generate_pkce();
        let b = generate_pkce();
        assert_ne!(a.verifier, b.verifier);
    }

    // -----------------------------------------------------------------------
    // Phase 2: State
    // -----------------------------------------------------------------------

    #[test]
    fn state_is_64_hex_chars() {
        let state = generate_state();
        assert_eq!(state.len(), 64);
        assert!(
            state.chars().all(|c| c.is_ascii_hexdigit()),
            "state contains non-hex chars: {state}"
        );
    }

    #[test]
    fn state_unique_per_call() {
        let a = generate_state();
        let b = generate_state();
        assert_ne!(a, b);
    }

    #[test]
    fn redacted_url_for_log_masks_sensitive_oauth_query_values() {
        assert_eq!(
            redacted_url_for_log("https://auth.example.test/oauth/token?code=abc&state=xyz&keep=1"),
            "https://auth.example.test/oauth/token?code=****&state=****&keep=1"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 3: Auth URL
    // -----------------------------------------------------------------------

    #[test]
    fn authorize_url_has_required_params() {
        let pkce = generate_pkce();
        let state = generate_state();
        let scope = "openid profile email offline_access";
        let url = build_authorize_url(
            "https://auth.openai.com",
            "test-client",
            "http://127.0.0.1:1455/callback",
            scope,
            &pkce,
            &state,
        );
        assert!(url.contains("response_type=code"), "missing response_type");
        assert!(url.contains("client_id=test-client"), "missing client_id");
        assert!(url.contains("redirect_uri="), "missing redirect_uri");
        assert!(
            url.contains(&format!("scope={}", percent_encode_param(scope))),
            "missing scope"
        );
        assert!(
            url.contains(&format!("code_challenge={}", pkce.challenge)),
            "missing code_challenge"
        );
        assert!(
            url.contains("code_challenge_method=S256"),
            "missing code_challenge_method"
        );
        assert!(url.contains(&format!("state={state}")), "missing state");
    }

    #[test]
    fn authorize_url_has_no_extra_params() {
        let pkce = generate_pkce();
        let state = generate_state();
        let url = build_authorize_url(
            "https://auth.openai.com",
            "test-client",
            "http://127.0.0.1:1455/callback",
            "openid profile email offline_access",
            &pkce,
            &state,
        );
        assert!(
            !url.contains("id_token_add_organizations"),
            "should not contain OpenAI-specific params"
        );
        assert!(
            !url.contains("codex_cli_simplified_flow"),
            "should not contain OpenAI-specific params"
        );
        assert!(
            !url.contains("originator"),
            "should not contain OpenAI-specific params"
        );
    }

    #[test]
    fn authorize_url_starts_with_issuer() {
        let pkce = generate_pkce();
        let state = generate_state();
        let url = build_authorize_url(
            "https://auth.openai.com",
            "test-client",
            "http://127.0.0.1:1455/callback",
            "openid profile email offline_access",
            &pkce,
            &state,
        );
        assert!(
            url.starts_with("https://auth.openai.com/oauth/authorize?"),
            "URL does not start with issuer: {url}"
        );
    }

    #[test]
    fn build_redirect_uri_constructs_expected_uri() {
        assert_eq!(
            build_redirect_uri(1455, "/auth/callback"),
            "http://127.0.0.1:1455/auth/callback"
        );
        assert_eq!(
            build_redirect_uri(8080, "/oauth/done"),
            "http://127.0.0.1:8080/oauth/done"
        );
        assert_eq!(build_redirect_uri(1, "/"), "http://127.0.0.1:1/");
    }

    // -----------------------------------------------------------------------
    // Phase 4: Token exchange
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn exchange_code_success() {
        let server = httpmock::MockServer::start_async().await;

        let mock = server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .form_urlencoded_tuple("grant_type", "authorization_code")
                    .form_urlencoded_tuple("client_id", "test-client")
                    .form_urlencoded_tuple("code", "test-code")
                    .form_urlencoded_tuple("redirect_uri", "http://localhost/cb")
                    .form_urlencoded_tuple("code_verifier", "test-verifier");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        serde_json::json!({
                            "id_token": "id-tok",
                            "access_token": "access-tok",
                            "refresh_token": "refresh-tok",
                            "expires_in": 3600
                        })
                        .to_string(),
                    );
            })
            .await;

        let tokens = exchange_code(
            OAuthEndpoint {
                token_url: &server.url("/oauth/token"),
                client_id: "test-client",
            },
            "test-code",
            Some("http://localhost/cb"),
            Some("test-verifier"),
        )
        .await
        .unwrap();

        assert_eq!(tokens.id_token.as_deref(), Some("id-tok"));
        assert_eq!(tokens.access_token, "access-tok");
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh-tok"));
        assert_eq!(tokens.expires_in, Some(3600));

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn exchange_code_allows_missing_optional_tokens() {
        let server = httpmock::MockServer::start_async().await;

        server
            .mock_async(|when, then| {
                when.method("POST").path("/oauth/token");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        serde_json::json!({
                            "access_token": "access-tok"
                        })
                        .to_string(),
                    );
            })
            .await;

        let tokens = exchange_code(
            OAuthEndpoint {
                token_url: &server.url("/oauth/token"),
                client_id: "test-client",
            },
            "test-code",
            Some("http://localhost/cb"),
            Some("test-verifier"),
        )
        .await
        .unwrap();

        assert_eq!(tokens.id_token, None);
        assert_eq!(tokens.access_token, "access-tok");
        assert_eq!(tokens.refresh_token, None);
        assert_eq!(tokens.expires_in, None);
    }

    #[tokio::test]
    async fn exchange_code_error_response() {
        let server = httpmock::MockServer::start_async().await;

        server
            .mock_async(|when, then| {
                when.method("POST").path("/oauth/token");
                then.status(400).body(r#"{"error": "invalid_grant"}"#);
            })
            .await;

        let err = exchange_code(
            OAuthEndpoint {
                token_url: &server.url("/oauth/token"),
                client_id: "test-client",
            },
            "bad-code",
            Some("http://localhost/cb"),
            Some("verifier"),
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("400"), "error should contain status: {err}");
    }

    #[tokio::test]
    async fn exchange_code_skips_optional_params_when_absent() {
        let server = httpmock::MockServer::start_async().await;

        let mock = server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .form_urlencoded_tuple("grant_type", "authorization_code")
                    .form_urlencoded_tuple("client_id", "test-client")
                    .form_urlencoded_tuple("code", "test-code");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        serde_json::json!({
                            "access_token": "access-tok"
                        })
                        .to_string(),
                    );
            })
            .await;

        let tokens = exchange_code(
            OAuthEndpoint {
                token_url: &server.url("/oauth/token"),
                client_id: "test-client",
            },
            "test-code",
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(tokens.access_token, "access-tok");
        mock.assert_async().await;
    }

    // -----------------------------------------------------------------------
    // Phase 5: Token refresh
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn refresh_token_success() {
        let server = httpmock::MockServer::start_async().await;

        let mock = server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/oauth/token")
                    .form_urlencoded_tuple("grant_type", "refresh_token")
                    .form_urlencoded_tuple("client_id", "test-client")
                    .form_urlencoded_tuple("refresh_token", "old-refresh-tok");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        serde_json::json!({
                            "id_token": "new-id",
                            "access_token": "new-access",
                            "refresh_token": "new-refresh",
                            "expires_in": 7200
                        })
                        .to_string(),
                    );
            })
            .await;

        let tokens = refresh_token(
            OAuthEndpoint {
                token_url: &server.url("/oauth/token"),
                client_id: "test-client",
            },
            "old-refresh-tok",
        )
        .await
        .unwrap();

        assert_eq!(tokens.id_token.as_deref(), Some("new-id"));
        assert_eq!(tokens.access_token, "new-access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh"));
        assert_eq!(tokens.expires_in, Some(7200));

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn refresh_token_error() {
        let server = httpmock::MockServer::start_async().await;

        server
            .mock_async(|when, then| {
                when.method("POST").path("/oauth/token");
                then.status(401).body(r#"{"error": "invalid_token"}"#);
            })
            .await;

        let err = refresh_token(
            OAuthEndpoint {
                token_url: &server.url("/oauth/token"),
                client_id: "test-client",
            },
            "expired-tok",
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("401"), "error should contain status: {err}");
    }

    // -----------------------------------------------------------------------
    // Phase 6: Callback server
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn callback_server_binds_ephemeral_port_and_receives_code() {
        let callback_path = "/custom/path";
        let (port, code_rx) = start_callback_server(0, callback_path, "test-state".to_string())
            .await
            .unwrap();

        assert_ne!(port, 0);

        let client = test_http_client();
        client
            .get(format!(
                "http://127.0.0.1:{port}{callback_path}?code=abc&state=test-state"
            ))
            .send()
            .await
            .unwrap();

        let code = code_rx.await.unwrap().unwrap();
        assert_eq!(code, "abc");
    }

    #[tokio::test]
    async fn callback_server_routes_non_default_path() {
        let callback_path = "/oauth/done";
        let (port, _code_rx) = start_callback_server(0, callback_path, "test-state".to_string())
            .await
            .unwrap();

        let client = test_http_client();
        let resp = client
            .get(format!(
                "http://127.0.0.1:{port}{callback_path}?code=abc&state=test-state"
            ))
            .send()
            .await
            .unwrap();

        assert_reqwest_status(resp, StatusCode::OK, "GET /oauth/done").await;
    }

    #[tokio::test]
    async fn callback_server_validates_state() {
        let callback_path = "/oauth/done";
        let (port, _code_rx) = start_callback_server(0, callback_path, "correct-state".to_string())
            .await
            .unwrap();

        let client = test_http_client();
        let resp = client
            .get(format!(
                "http://127.0.0.1:{port}{callback_path}?code=abc&state=wrong-state"
            ))
            .send()
            .await
            .unwrap();

        assert_reqwest_status(resp, StatusCode::BAD_REQUEST, "GET /oauth/done").await;
    }

    #[tokio::test]
    async fn callback_server_returns_success_html() {
        let callback_path = "/oauth/done";
        let (port, _code_rx) = start_callback_server(0, callback_path, "test-state".to_string())
            .await
            .unwrap();

        let client = test_http_client();
        let resp = client
            .get(format!(
                "http://127.0.0.1:{port}{callback_path}?code=abc&state=test-state"
            ))
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert!(
            body.contains("<title>Signed in · Fabro</title>"),
            "response should contain the new title: {body}"
        );
        assert!(
            body.contains("You're signed in to Fabro"),
            "response should contain the success headline: {body}"
        );
    }

    #[tokio::test]
    async fn callback_server_rejects_invalid_paths() {
        for path in ["", "no-leading-slash", "/:param", "/*wildcard"] {
            let err = start_callback_server(0, path, "test-state".to_string())
                .await
                .unwrap_err()
                .to_string();
            assert!(!err.is_empty(), "expected error for path {path}");
        }
    }

    #[tokio::test]
    async fn callback_server_with_errors_forwards_oauth_error() {
        let callback_path = "/oauth/done";
        let (handle, callback_rx) =
            start_callback_server_with_errors("test-state".to_string(), 0, callback_path)
                .await
                .unwrap();

        let client = test_http_client();
        let response = client
            .get(format!(
                "http://127.0.0.1:{}{callback_path}?error=access_denied&error_description=Authorization%20denied&state=test-state",
                handle.port()
            ))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let callback = callback_rx.await.unwrap().unwrap_err();
        assert_eq!(callback.error_code, "access_denied");
        assert_eq!(callback.error_description, "Authorization denied");
    }

    #[tokio::test]
    async fn callback_server_with_errors_ignores_state_mismatch_until_real_callback_arrives() {
        let callback_path = "/oauth/done";
        let (handle, callback_rx) =
            start_callback_server_with_errors("correct-state".to_string(), 0, callback_path)
                .await
                .unwrap();

        let client = test_http_client();
        let mismatch = client
            .get(format!(
                "http://127.0.0.1:{}{callback_path}?error=access_denied&error_description=boom&state=wrong-state",
                handle.port()
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(mismatch.status(), StatusCode::BAD_REQUEST);

        let success = client
            .get(format!(
                "http://127.0.0.1:{}{callback_path}?code=auth-code&state=correct-state",
                handle.port()
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(success.status(), StatusCode::OK);

        let callback = callback_rx.await.unwrap().unwrap();
        assert_eq!(callback.code, "auth-code");
    }
}
