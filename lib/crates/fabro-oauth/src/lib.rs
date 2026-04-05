use std::fmt::Write as _;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

// ---------------------------------------------------------------------------
// PKCE
// ---------------------------------------------------------------------------

pub struct PkceCodes {
    pub verifier: String,
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

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub id_token: Option<String>,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
}

// ---------------------------------------------------------------------------
// Token exchange
// ---------------------------------------------------------------------------

pub async fn exchange_code_for_tokens(
    client: &reqwest::Client,
    issuer: &str,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<TokenResponse, String> {
    tracing::debug!(issuer, "Exchanging authorization code");

    let body = encode_form(&[
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", code_verifier),
    ]);

    let url = format!("{issuer}/oauth/token");
    let resp = client
        .post(&url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Token exchange request failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        tracing::error!(%status, "Token exchange failed");
        return Err(format!("Token exchange failed ({status}): {body_text}"));
    }

    let tokens: TokenResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {e}"))?;

    tracing::info!(expires_in = ?tokens.expires_in, "Token exchange completed");
    Ok(tokens)
}

// ---------------------------------------------------------------------------
// Token refresh
// ---------------------------------------------------------------------------

pub async fn refresh_access_token(
    client: &reqwest::Client,
    issuer: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenResponse, String> {
    tracing::debug!(issuer, "Refreshing access token");

    let body = encode_form(&[
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", refresh_token),
    ]);

    let url = format!("{issuer}/oauth/token");
    let resp = client
        .post(&url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Token refresh request failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        tracing::warn!(%status, "Token refresh failed");
        return Err(format!("Token refresh failed ({status}): {body_text}"));
    }

    let tokens: TokenResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse refresh token response: {e}"))?;

    tracing::info!(expires_in = ?tokens.expires_in, "Token refreshed");
    Ok(tokens)
}

// ---------------------------------------------------------------------------
// Callback server
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CallbackParams {
    code: Option<String>,
    state: String,
    error: Option<String>,
    error_description: Option<String>,
}

fn validate_callback_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("Callback path must not be empty".to_string());
    }
    if !path.starts_with('/') {
        return Err(format!("Callback path must start with '/': {path}"));
    }
    if path
        .split('/')
        .skip(1)
        .any(|segment| segment.starts_with(':') || segment.starts_with('*'))
    {
        return Err(format!(
            "Callback path must not contain route parameters: {path}"
        ));
    }
    Ok(())
}

fn build_redirect_uri(port: u16, path: &str) -> String {
    format!("http://127.0.0.1:{port}{path}")
}

pub async fn start_callback_server(
    port: u16,
    path: &str,
    expected_state: String,
) -> Result<(u16, oneshot::Receiver<Result<String, String>>), String> {
    validate_callback_path(path)?;

    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| format!("Failed to bind callback server: {e}"))?;
    let actual_port = listener
        .local_addr()
        .map_err(|e| format!("Failed to get local address: {e}"))?
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
                        Html("State mismatch".to_string()),
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
                        Html(format!(
                            r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>Authorization Failed</title>
<style>
  body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; display: flex; justify-content: center; align-items: center; min-height: 100vh; margin: 0; background: #f6f8fa; color: #1f2328; }}
  .card {{ text-align: center; background: #fff; border: 1px solid #d1d9e0; border-radius: 12px; padding: 48px; max-width: 420px; }}
  .icon {{ font-size: 48px; margin-bottom: 16px; }}
  h1 {{ font-size: 20px; font-weight: 600; margin: 0 0 8px; }}
  p {{ font-size: 14px; color: #59636e; margin: 0; }}
</style>
</head>
<body>
<div class="card">
  <div class="icon">&#10007;</div>
  <h1>Authorization Failed</h1>
  <p>{desc}</p>
</div>
</body>
</html>"#
                        )),
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
                        Html("No authorization code received".to_string()),
                    );
                };

                if let Some(tx) = code_tx.lock().unwrap().take() {
                    let _ = tx.send(Ok(code));
                }
                if let Some(tx) = shutdown_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                (
                    StatusCode::OK,
                    Html(
                        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>Authorization</title>
<style>
  body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; display: flex; justify-content: center; align-items: center; min-height: 100vh; margin: 0; background: #f6f8fa; color: #1f2328; }
  .card { text-align: center; background: #fff; border: 1px solid #d1d9e0; border-radius: 12px; padding: 48px; max-width: 420px; }
  .check { font-size: 48px; margin-bottom: 16px; }
  h1 { font-size: 20px; font-weight: 600; margin: 0 0 8px; }
  p { font-size: 14px; color: #59636e; margin: 0; }
</style>
</head>
<body>
<div class="card">
  <div class="check">&#10003;</div>
  <h1>Authorization Successful</h1>
  <p>You can close this tab and return to your terminal.</p>
</div>
</body>
</html>"#
                            .to_string(),
                    ),
                )
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

// ---------------------------------------------------------------------------
// Browser flow
// ---------------------------------------------------------------------------

pub async fn run_browser_flow(
    issuer: &str,
    client_id: &str,
    scope: &str,
    port: u16,
    callback_path: &str,
) -> Result<TokenResponse, String> {
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

    if let Err(e) = open::that(&auth_url) {
        tracing::warn!("Could not open browser: {e}");
    }

    let code = code_rx
        .await
        .map_err(|_| "Did not receive authorization code".to_string())?
        .map_err(|e| format!("Authorization failed: {e}"))?;

    let client = reqwest::Client::new();
    exchange_code_for_tokens(
        &client,
        issuer,
        client_id,
        &code,
        &redirect_uri,
        &pkce.verifier,
    )
    .await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_http_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
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

        let client = test_http_client();
        let tokens = exchange_code_for_tokens(
            &client,
            &server.url(""),
            "test-client",
            "test-code",
            "http://localhost/cb",
            "test-verifier",
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

        let client = test_http_client();
        let tokens = exchange_code_for_tokens(
            &client,
            &server.url(""),
            "test-client",
            "test-code",
            "http://localhost/cb",
            "test-verifier",
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

        let client = test_http_client();
        let err = exchange_code_for_tokens(
            &client,
            &server.url(""),
            "test-client",
            "bad-code",
            "http://localhost/cb",
            "verifier",
        )
        .await
        .unwrap_err();

        assert!(err.contains("400"), "error should contain status: {err}");
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

        let client = test_http_client();
        let tokens =
            refresh_access_token(&client, &server.url(""), "test-client", "old-refresh-tok")
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

        let client = test_http_client();
        let err = refresh_access_token(&client, &server.url(""), "test-client", "expired-tok")
            .await
            .unwrap_err();

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

        assert_eq!(resp.status(), 200);
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

        assert_eq!(resp.status(), 400);
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
            body.contains("<title>Authorization</title>"),
            "response should contain updated title: {body}"
        );
        assert!(
            body.contains("Authorization Successful"),
            "response should contain success message: {body}"
        );
    }

    #[tokio::test]
    async fn callback_server_rejects_invalid_paths() {
        for path in ["", "no-leading-slash", "/:param", "/*wildcard"] {
            let err = start_callback_server(0, path, "test-state".to_string())
                .await
                .unwrap_err();
            assert!(!err.is_empty(), "expected error for path {path}");
        }
    }
}
