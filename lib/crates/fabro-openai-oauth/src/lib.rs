use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const DEFAULT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const DEFAULT_ISSUER: &str = "https://auth.openai.com";
pub const OAUTH_PORT: u16 = 1455;

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
                out.push_str(&format!("%{b:02X}"));
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
    pkce: &PkceCodes,
    state: &str,
) -> String {
    let params = encode_form(&[
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("scope", "openid profile email offline_access"),
        ("code_challenge", &pkce.challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "fabro"),
    ]);
    format!("{issuer}/oauth/authorize?{params}")
}

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: Option<u64>,
}

// ---------------------------------------------------------------------------
// JWT claims
// ---------------------------------------------------------------------------

pub struct IdTokenClaims {
    pub chatgpt_account_id: Option<String>,
}

#[derive(Deserialize)]
struct JwtPayload {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
    #[serde(default, rename = "https://api.openai.com/auth")]
    auth_claim: Option<AuthClaim>,
    #[serde(default)]
    organizations: Option<Vec<Organization>>,
}

#[derive(Deserialize)]
struct AuthClaim {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
}

#[derive(Deserialize)]
struct Organization {
    #[serde(default)]
    id: Option<String>,
}

fn parse_jwt_payload(token: &str) -> Option<JwtPayload> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload_bytes = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    serde_json::from_slice(&payload_bytes).ok()
}

pub fn parse_jwt_claims(token: &str) -> Option<IdTokenClaims> {
    let payload = parse_jwt_payload(token)?;
    let chatgpt_account_id = payload
        .chatgpt_account_id
        .or_else(|| payload.auth_claim.and_then(|a| a.chatgpt_account_id));
    Some(IdTokenClaims { chatgpt_account_id })
}

pub fn extract_account_id(tokens: &TokenResponse) -> Option<String> {
    let payload = parse_jwt_payload(&tokens.id_token)?;
    payload
        .chatgpt_account_id
        .or_else(|| payload.auth_claim.and_then(|a| a.chatgpt_account_id))
        .or_else(|| {
            payload
                .organizations
                .and_then(|orgs| orgs.into_iter().next())
                .and_then(|org| org.id)
        })
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
// Device flow
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct DeviceAuthResponse {
    pub device_auth_id: String,
    pub user_code: String,
    pub interval: u64,
}

pub async fn initiate_device_flow(
    client: &reqwest::Client,
    issuer: &str,
    client_id: &str,
) -> Result<DeviceAuthResponse, String> {
    let url = format!("{issuer}/api/accounts/deviceauth/usercode");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "client_id": client_id }))
        .send()
        .await
        .map_err(|e| format!("Device flow initiation failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "Device flow initiation failed ({status}): {body_text}"
        ));
    }

    let device: DeviceAuthResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse device flow response: {e}"))?;

    tracing::info!("Device flow initiated");
    Ok(device)
}

#[derive(Deserialize)]
struct DevicePollResponse {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

pub async fn poll_device_flow(
    client: &reqwest::Client,
    issuer: &str,
    client_id: &str,
    device: &DeviceAuthResponse,
) -> Result<TokenResponse, String> {
    let poll_url = format!("{issuer}/api/accounts/deviceauth/token");
    let redirect_uri = format!("http://localhost:{OAUTH_PORT}/auth/callback");
    let mut attempt = 0u32;

    loop {
        attempt += 1;
        let resp = client
            .post(&poll_url)
            .json(&serde_json::json!({
                "client_id": client_id,
                "device_auth_id": device.device_auth_id,
            }))
            .send()
            .await
            .map_err(|e| format!("Device flow poll failed: {e}"))?;

        let poll: DevicePollResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse device poll response: {e}"))?;

        if let Some(code) = poll.code {
            tracing::info!("Device flow completed");
            return exchange_code_for_tokens(client, issuer, client_id, &code, &redirect_uri, "")
                .await;
        }

        if let Some(ref error) = poll.error {
            if error == "authorization_pending" {
                tracing::debug!(attempt, "Device flow authorization pending");
                if device.interval > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(device.interval)).await;
                }
                continue;
            }
            if error == "expired_token" {
                tracing::error!("Device flow expired");
                return Err("Device flow authorization expired".to_string());
            }
            return Err(format!("Device flow error: {error}"));
        }

        return Err("Unexpected device poll response".to_string());
    }
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

pub async fn start_callback_server(
    port: u16,
    expected_state: String,
) -> Result<(u16, tokio::sync::oneshot::Receiver<Result<String, String>>), String> {
    let listener = tokio::net::TcpListener::bind(format!("localhost:{port}"))
        .await
        .map_err(|e| format!("Failed to bind callback server: {e}"))?;
    let actual_port = listener
        .local_addr()
        .map_err(|e| format!("Failed to get local address: {e}"))?
        .port();

    let (code_tx, code_rx) = tokio::sync::oneshot::channel::<Result<String, String>>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let code_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(code_tx)));
    let shutdown_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(shutdown_tx)));
    let expected_state = std::sync::Arc::new(expected_state);

    let app = axum::Router::new().route(
        "/auth/callback",
        axum::routing::get(
            move |axum::extract::Query(params): axum::extract::Query<CallbackParams>| async move {
                if params.state != *expected_state {
                    return (
                        axum::http::StatusCode::BAD_REQUEST,
                        axum::response::Html("State mismatch".to_string()),
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
                        axum::http::StatusCode::BAD_REQUEST,
                        axum::response::Html(format!(
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

                let code = match params.code {
                    Some(c) => c,
                    None => {
                        if let Some(tx) = code_tx.lock().unwrap().take() {
                            let _ = tx.send(Err("No authorization code received".to_string()));
                        }
                        if let Some(tx) = shutdown_tx.lock().unwrap().take() {
                            let _ = tx.send(());
                        }
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            axum::response::Html("No authorization code received".to_string()),
                        );
                    }
                };

                if let Some(tx) = code_tx.lock().unwrap().take() {
                    let _ = tx.send(Ok(code));
                }
                if let Some(tx) = shutdown_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                (
                    axum::http::StatusCode::OK,
                    axum::response::Html(
                        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>Arc</title>
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

pub async fn run_browser_flow(issuer: &str, client_id: &str) -> Result<TokenResponse, String> {
    let pkce = generate_pkce();
    let state = generate_state();
    let redirect_uri = format!("http://localhost:{OAUTH_PORT}/auth/callback");

    let (_port, code_rx) = start_callback_server(OAUTH_PORT, state.clone()).await?;
    let auth_url = build_authorize_url(issuer, client_id, &redirect_uri, &pkce, &state);

    tracing::info!(port = OAUTH_PORT, "OAuth browser flow started");

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
        let url = build_authorize_url(
            "https://auth.openai.com",
            "test-client",
            "http://127.0.0.1:1455/callback",
            &pkce,
            &state,
        );
        assert!(url.contains("response_type=code"), "missing response_type");
        assert!(url.contains("client_id=test-client"), "missing client_id");
        assert!(url.contains("redirect_uri="), "missing redirect_uri");
        assert!(url.contains("scope="), "missing scope");
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
    fn authorize_url_has_openai_params() {
        let pkce = generate_pkce();
        let state = generate_state();
        let url = build_authorize_url(
            "https://auth.openai.com",
            "test-client",
            "http://127.0.0.1:1455/callback",
            &pkce,
            &state,
        );
        assert!(
            url.contains("id_token_add_organizations=true"),
            "missing id_token_add_organizations"
        );
        assert!(
            url.contains("codex_cli_simplified_flow=true"),
            "missing codex_cli_simplified_flow"
        );
        assert!(url.contains("originator=fabro"), "missing originator");
    }

    #[test]
    fn authorize_url_starts_with_issuer() {
        let pkce = generate_pkce();
        let state = generate_state();
        let url = build_authorize_url(
            "https://auth.openai.com",
            "test-client",
            "http://127.0.0.1:1455/callback",
            &pkce,
            &state,
        );
        assert!(
            url.starts_with("https://auth.openai.com/oauth/authorize?"),
            "URL does not start with issuer: {url}"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 4: JWT claims
    // -----------------------------------------------------------------------

    fn make_test_jwt(claims: &serde_json::Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_string(claims).unwrap());
        format!("{header}.{payload}.signature")
    }

    #[test]
    fn parse_jwt_with_chatgpt_account_id() {
        let jwt = make_test_jwt(&serde_json::json!({
            "chatgpt_account_id": "acct_123"
        }));
        let claims = parse_jwt_claims(&jwt).unwrap();
        assert_eq!(claims.chatgpt_account_id.as_deref(), Some("acct_123"));
    }

    #[test]
    fn parse_jwt_with_nested_auth_claim() {
        let jwt = make_test_jwt(&serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_nested"
            }
        }));
        let claims = parse_jwt_claims(&jwt).unwrap();
        assert_eq!(claims.chatgpt_account_id.as_deref(), Some("acct_nested"));
    }

    #[test]
    fn parse_jwt_with_organizations() {
        let jwt = make_test_jwt(&serde_json::json!({
            "organizations": [{"id": "org_456"}]
        }));
        let tokens = TokenResponse {
            id_token: jwt,
            access_token: String::new(),
            refresh_token: String::new(),
            expires_in: None,
        };
        assert_eq!(extract_account_id(&tokens).as_deref(), Some("org_456"));
    }

    #[test]
    fn parse_jwt_invalid_format() {
        assert!(parse_jwt_claims("not-a-jwt").is_none());
    }

    #[test]
    fn parse_jwt_invalid_base64() {
        assert!(parse_jwt_claims("header.!!!invalid!!!.sig").is_none());
    }

    #[test]
    fn extract_account_id_prefers_top_level() {
        let jwt = make_test_jwt(&serde_json::json!({
            "chatgpt_account_id": "top_level",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "nested"
            },
            "organizations": [{"id": "org"}]
        }));
        let tokens = TokenResponse {
            id_token: jwt,
            access_token: String::new(),
            refresh_token: String::new(),
            expires_in: None,
        };
        assert_eq!(extract_account_id(&tokens).as_deref(), Some("top_level"));
    }

    #[test]
    fn extract_account_id_falls_back_to_nested() {
        let jwt = make_test_jwt(&serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "nested"
            }
        }));
        let tokens = TokenResponse {
            id_token: jwt,
            access_token: String::new(),
            refresh_token: String::new(),
            expires_in: None,
        };
        assert_eq!(extract_account_id(&tokens).as_deref(), Some("nested"));
    }

    #[test]
    fn extract_account_id_none_when_missing() {
        let jwt = make_test_jwt(&serde_json::json!({}));
        let tokens = TokenResponse {
            id_token: jwt,
            access_token: String::new(),
            refresh_token: String::new(),
            expires_in: None,
        };
        assert!(extract_account_id(&tokens).is_none());
    }

    // -----------------------------------------------------------------------
    // Phase 5: Token exchange
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn exchange_code_success() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/oauth/token")
            .match_header("content-type", "application/x-www-form-urlencoded")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "grant_type".to_string(),
                    "authorization_code".to_string(),
                ),
                mockito::Matcher::UrlEncoded("client_id".to_string(), "test-client".to_string()),
                mockito::Matcher::UrlEncoded("code".to_string(), "test-code".to_string()),
                mockito::Matcher::UrlEncoded(
                    "redirect_uri".to_string(),
                    "http://localhost/cb".to_string(),
                ),
                mockito::Matcher::UrlEncoded(
                    "code_verifier".to_string(),
                    "test-verifier".to_string(),
                ),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id_token": "id-tok",
                    "access_token": "access-tok",
                    "refresh_token": "refresh-tok",
                    "expires_in": 3600
                })
                .to_string(),
            )
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let tokens = exchange_code_for_tokens(
            &client,
            &server.url(),
            "test-client",
            "test-code",
            "http://localhost/cb",
            "test-verifier",
        )
        .await
        .unwrap();

        assert_eq!(tokens.id_token, "id-tok");
        assert_eq!(tokens.access_token, "access-tok");
        assert_eq!(tokens.refresh_token, "refresh-tok");
        assert_eq!(tokens.expires_in, Some(3600));

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn exchange_code_error_response() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("POST", "/oauth/token")
            .with_status(400)
            .with_body(r#"{"error": "invalid_grant"}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let err = exchange_code_for_tokens(
            &client,
            &server.url(),
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
    // Phase 6: Token refresh
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn refresh_token_success() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/oauth/token")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("grant_type".to_string(), "refresh_token".to_string()),
                mockito::Matcher::UrlEncoded("client_id".to_string(), "test-client".to_string()),
                mockito::Matcher::UrlEncoded(
                    "refresh_token".to_string(),
                    "old-refresh-tok".to_string(),
                ),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id_token": "new-id",
                    "access_token": "new-access",
                    "refresh_token": "new-refresh",
                    "expires_in": 7200
                })
                .to_string(),
            )
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let tokens = refresh_access_token(&client, &server.url(), "test-client", "old-refresh-tok")
            .await
            .unwrap();

        assert_eq!(tokens.access_token, "new-access");
        assert_eq!(tokens.refresh_token, "new-refresh");
        assert_eq!(tokens.expires_in, Some(7200));

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn refresh_token_error() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("POST", "/oauth/token")
            .with_status(401)
            .with_body(r#"{"error": "invalid_token"}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let err = refresh_access_token(&client, &server.url(), "test-client", "expired-tok")
            .await
            .unwrap_err();

        assert!(err.contains("401"), "error should contain status: {err}");
    }

    // -----------------------------------------------------------------------
    // Phase 7: Device flow
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn initiate_device_flow_success() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/api/accounts/deviceauth/usercode")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "device_auth_id": "dev-123",
                    "user_code": "ABCD-1234",
                    "interval": 5
                })
                .to_string(),
            )
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let device = initiate_device_flow(&client, &server.url(), "test-client")
            .await
            .unwrap();

        assert_eq!(device.device_auth_id, "dev-123");
        assert_eq!(device.user_code, "ABCD-1234");
        assert_eq!(device.interval, 5);

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn initiate_device_flow_error() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("POST", "/api/accounts/deviceauth/usercode")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let err = initiate_device_flow(&client, &server.url(), "test-client")
            .await
            .unwrap_err();

        assert!(err.contains("500"), "error should contain status: {err}");
    }

    #[tokio::test]
    async fn poll_device_flow_success() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("POST", "/api/accounts/deviceauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(serde_json::json!({"code": "auth-code-123"}).to_string())
            .create_async()
            .await;

        server
            .mock("POST", "/oauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id_token": "dev-id",
                    "access_token": "dev-access",
                    "refresh_token": "dev-refresh",
                    "expires_in": 3600
                })
                .to_string(),
            )
            .create_async()
            .await;

        let device = DeviceAuthResponse {
            device_auth_id: "dev-123".to_string(),
            user_code: "ABCD-1234".to_string(),
            interval: 0,
        };

        let client = reqwest::Client::new();
        let tokens = poll_device_flow(&client, &server.url(), "test-client", &device)
            .await
            .unwrap();

        assert_eq!(tokens.access_token, "dev-access");
    }

    #[tokio::test]
    async fn poll_device_flow_expired() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("POST", "/api/accounts/deviceauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(serde_json::json!({"error": "expired_token"}).to_string())
            .create_async()
            .await;

        let device = DeviceAuthResponse {
            device_auth_id: "dev-expired".to_string(),
            user_code: "XXXX-0000".to_string(),
            interval: 0,
        };

        let client = reqwest::Client::new();
        let err = poll_device_flow(&client, &server.url(), "test-client", &device)
            .await
            .unwrap_err();

        assert!(
            err.contains("expired"),
            "error should mention expiry: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 8: Callback server
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn callback_server_receives_code() {
        let (port, code_rx) = start_callback_server(0, "test-state".to_string())
            .await
            .unwrap();

        let client = reqwest::Client::new();
        client
            .get(format!(
                "http://localhost:{port}/auth/callback?code=abc&state=test-state"
            ))
            .send()
            .await
            .unwrap();

        let code = code_rx.await.unwrap().unwrap();
        assert_eq!(code, "abc");
    }

    #[tokio::test]
    async fn callback_server_validates_state() {
        let (port, _code_rx) = start_callback_server(0, "correct-state".to_string())
            .await
            .unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .get(format!(
                "http://localhost:{port}/auth/callback?code=abc&state=wrong-state"
            ))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn callback_server_returns_success_html() {
        let (port, _code_rx) = start_callback_server(0, "test-state".to_string())
            .await
            .unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .get(format!(
                "http://localhost:{port}/auth/callback?code=abc&state=test-state"
            ))
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert!(
            body.contains("Authorization Successful"),
            "response should contain success message: {body}"
        );
    }
}
