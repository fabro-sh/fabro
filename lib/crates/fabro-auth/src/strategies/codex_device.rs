use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use fabro_http::HttpClient;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::sleep;

use crate::context::{AuthContextRequest, AuthContextResponse};
use crate::credential::{
    AuthCredential, AuthDetails, OAuthConfig, OAuthTokens, expires_at_from_now,
};
use crate::strategy::AuthStrategy;

const DEVICE_AUTH_POLL_INTERVAL: Duration = Duration::from_secs(2);
const CODEX_DEVICE_VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";

fn http_client() -> anyhow::Result<HttpClient> {
    #[cfg(test)]
    {
        fabro_http::test_http_client().map_err(anyhow::Error::from)
    }
    #[cfg(not(test))]
    {
        fabro_http::http_client().map_err(anyhow::Error::from)
    }
}

fn join_url(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

#[derive(Debug, Deserialize)]
struct JwtPayload {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
    #[serde(default, rename = "https://api.openai.com/auth")]
    auth_claim: Option<AuthClaim>,
    #[serde(default)]
    organizations: Option<Vec<Organization>>,
}

#[derive(Debug, Deserialize)]
struct AuthClaim {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
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

pub fn extract_chatgpt_account_id(id_token: &str) -> Option<String> {
    let payload = parse_jwt_payload(id_token)?;
    payload
        .chatgpt_account_id
        .or_else(|| {
            payload
                .auth_claim
                .and_then(|claim| claim.chatgpt_account_id)
        })
        .or_else(|| {
            payload
                .organizations
                .and_then(|orgs| orgs.into_iter().next())
                .and_then(|org| org.id)
        })
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum U64OrString {
    U64(u64),
    String(String),
}

impl U64OrString {
    fn as_u64(&self) -> Option<u64> {
        match self {
            Self::U64(value) => Some(*value),
            Self::String(value) => value.parse().ok(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct DeviceCodeInitResponse {
    device_auth_id: String,
    user_code: String,
    #[serde(default)]
    interval: Option<U64OrString>,
    #[serde(default)]
    expires_in: Option<U64OrString>,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct DeviceCodePollResponse {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    authorization_code: Option<String>,
    #[serde(default)]
    code_verifier: Option<String>,
}

#[derive(Debug, Serialize)]
struct DeviceCodeInitRequest<'a> {
    client_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
}

#[derive(Debug)]
struct PendingDeviceAuth {
    device_auth_id: String,
    user_code: String,
    poll_interval: Duration,
    deadline: Instant,
}

#[derive(Debug)]
struct DeviceAuthorization {
    authorization_code: String,
    code_verifier: String,
}

pub struct CodexDeviceStrategy {
    config: OAuthConfig,
    pending: Option<PendingDeviceAuth>,
}

impl CodexDeviceStrategy {
    #[must_use]
    pub fn new(config: OAuthConfig) -> Self {
        Self {
            config,
            pending: None,
        }
    }

    fn init_expiry_seconds(payload: &DeviceCodeInitResponse) -> u64 {
        if let Some(expires_at) = payload.expires_at {
            let seconds = expires_at.signed_duration_since(Utc::now()).num_seconds();
            if seconds > 0 {
                return seconds.cast_unsigned();
            }
        }
        if let Some(seconds) = payload.expires_in.as_ref().and_then(U64OrString::as_u64) {
            return seconds;
        }
        300
    }

    fn init_poll_interval(payload: &DeviceCodeInitResponse) -> Duration {
        Duration::from_secs(
            payload
                .interval
                .as_ref()
                .and_then(U64OrString::as_u64)
                .unwrap_or(DEVICE_AUTH_POLL_INTERVAL.as_secs()),
        )
    }

    async fn poll_codex_device(
        &self,
        pending: &PendingDeviceAuth,
    ) -> anyhow::Result<DeviceAuthorization> {
        let client = http_client()?;
        let url = join_url(&self.config.auth_url, "/api/accounts/deviceauth/token");

        loop {
            if Instant::now() >= pending.deadline {
                return Err(anyhow::anyhow!("device auth timed out after 15 minutes"));
            }

            let response = client
                .post(&url)
                .header("originator", "fabro")
                .json(&json!({
                    "device_auth_id": pending.device_auth_id,
                    "user_code": pending.user_code,
                }))
                .send()
                .await?;
            let status = response.status();
            if status.as_u16() == 403 || status.as_u16() == 404 {
                sleep(pending.poll_interval).await;
                continue;
            }
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(anyhow::anyhow!(
                    "device auth failed with status {status}: {body}"
                ));
            }

            let payload: DeviceCodePollResponse = response.json().await?;
            if let (Some(authorization_code), Some(code_verifier)) =
                (payload.authorization_code, payload.code_verifier)
            {
                return Ok(DeviceAuthorization {
                    authorization_code,
                    code_verifier,
                });
            }

            match payload.status.as_deref() {
                Some("pending" | "running") => {
                    sleep(pending.poll_interval).await;
                }
                Some(other) => {
                    return Err(anyhow::anyhow!("device code exchange failed: {other}"));
                }
                None => {
                    return Err(anyhow::anyhow!(
                        "device auth response missing authorization_code or code_verifier"
                    ));
                }
            }
        }
    }
}

#[async_trait]
impl AuthStrategy for CodexDeviceStrategy {
    async fn init(&mut self) -> anyhow::Result<AuthContextRequest> {
        let client = http_client()?;
        let url = join_url(&self.config.auth_url, "/api/accounts/deviceauth/usercode");
        let response = client
            .post(&url)
            .header("originator", "fabro")
            .json(&DeviceCodeInitRequest {
                client_id: &self.config.client_id,
                scope: (!self.config.scopes.is_empty()).then(|| self.config.scopes.join(" ")),
            })
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "device code request failed with status {status}: {body}"
            ));
        }

        let payload: DeviceCodeInitResponse = response.json().await?;
        let expires_in = Self::init_expiry_seconds(&payload);
        let poll_interval = Self::init_poll_interval(&payload);
        self.pending = Some(PendingDeviceAuth {
            device_auth_id: payload.device_auth_id,
            user_code: payload.user_code.clone(),
            poll_interval,
            deadline: Instant::now() + Duration::from_secs(expires_in),
        });

        Ok(AuthContextRequest::DeviceCode {
            user_code: payload.user_code,
            verification_uri: CODEX_DEVICE_VERIFICATION_URI.to_string(),
            expires_in,
        })
    }

    async fn complete(&mut self, response: AuthContextResponse) -> anyhow::Result<AuthCredential> {
        match response {
            AuthContextResponse::ApiKey { .. } => Err(anyhow::anyhow!(
                "expected device code confirmation response"
            )),
            AuthContextResponse::DeviceCodeConfirmed => {
                let pending = self
                    .pending
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("device auth flow was not initialized"))?;
                let authorization = self.poll_codex_device(&pending).await?;
                let token_response = fabro_oauth::exchange_code(
                    fabro_oauth::OAuthEndpoint {
                        token_url: &self.config.token_url,
                        client_id: &self.config.client_id,
                    },
                    &authorization.authorization_code,
                    self.config.redirect_uri.as_deref(),
                    Some(&authorization.code_verifier),
                )
                .await
                .map_err(anyhow::Error::msg)?;

                Ok(AuthCredential {
                    provider: fabro_model::Provider::OpenAi,
                    details: AuthDetails::CodexOAuth {
                        tokens: OAuthTokens {
                            access_token: token_response.access_token,
                            refresh_token: token_response.refresh_token,
                            expires_at: expires_at_from_now(token_response.expires_in),
                        },
                        config: self.config.clone(),
                        account_id: token_response
                            .id_token
                            .as_deref()
                            .and_then(extract_chatgpt_account_id),
                    },
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use httpmock::Method::POST;
    use httpmock::MockServer;

    use super::*;
    use crate::strategy::codex_oauth_config;

    fn test_config(server: &MockServer) -> OAuthConfig {
        OAuthConfig {
            auth_url: server.url(""),
            token_url: server.url("/oauth/token"),
            client_id: "test-client".to_string(),
            scopes: vec![
                "openid".to_string(),
                "profile".to_string(),
                "email".to_string(),
                "offline_access".to_string(),
            ],
            redirect_uri: Some("https://auth.openai.com/deviceauth/callback".to_string()),
            use_pkce: false,
        }
    }

    fn make_test_jwt(claims: &serde_json::Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_string(claims).unwrap());
        format!("{header}.{payload}.signature")
    }

    #[test]
    fn extract_chatgpt_account_id_prefers_top_level_claim() {
        let jwt = make_test_jwt(&json!({
            "chatgpt_account_id": "top_level",
            "https://api.openai.com/auth": { "chatgpt_account_id": "nested" },
            "organizations": [{ "id": "org_123" }]
        }));
        assert_eq!(
            extract_chatgpt_account_id(&jwt).as_deref(),
            Some("top_level")
        );
    }

    #[test]
    fn extract_chatgpt_account_id_falls_back_to_nested_claim() {
        let jwt = make_test_jwt(&json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "nested" }
        }));
        assert_eq!(extract_chatgpt_account_id(&jwt).as_deref(), Some("nested"));
    }

    #[test]
    fn extract_chatgpt_account_id_falls_back_to_organization() {
        let jwt = make_test_jwt(&json!({
            "organizations": [{ "id": "org_123" }]
        }));
        assert_eq!(extract_chatgpt_account_id(&jwt).as_deref(), Some("org_123"));
    }

    #[test]
    fn codex_oauth_config_disables_pkce() {
        let config = codex_oauth_config();
        assert!(!config.use_pkce);
    }

    #[tokio::test]
    async fn init_accepts_live_openai_shape_and_uses_hardcoded_verification_url() {
        let server = MockServer::start_async().await;
        let init_mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/api/accounts/deviceauth/usercode")
                    .header("content-type", "application/json")
                    .header("originator", "fabro")
                    .json_body(serde_json::json!({
                        "client_id": "test-client",
                        "scope": "openid profile email offline_access"
                    }));
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        serde_json::json!({
                            "device_auth_id": "deviceauth_123",
                            "user_code": "ABCD-EFGH",
                            "interval": "5",
                            "expires_at": "2026-04-13T15:00:23.011951+00:00"
                        })
                        .to_string(),
                    );
            })
            .await;

        let mut strategy = CodexDeviceStrategy::new(test_config(&server));

        let request = strategy.init().await.unwrap();

        assert_eq!(
            request,
            AuthContextRequest::DeviceCode {
                user_code: "ABCD-EFGH".to_string(),
                verification_uri: "https://auth.openai.com/codex/device".to_string(),
                expires_in: 300,
            }
        );
        init_mock.assert_async().await;
    }

    #[tokio::test]
    async fn complete_retries_403_and_uses_server_code_verifier() {
        let server = MockServer::start_async().await;
        let init_mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/api/accounts/deviceauth/usercode")
                    .header("originator", "fabro")
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({
                        "client_id": "test-client",
                        "scope": "openid profile email offline_access"
                    }));
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        serde_json::json!({
                            "device_auth_id": "deviceauth_123",
                            "user_code": "ABCD-EFGH",
                            "interval": "0",
                            "expires_at": "2026-04-13T15:00:23.011951+00:00"
                        })
                        .to_string(),
                    );
            })
            .await;
        let pending_poll_mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/api/accounts/deviceauth/token")
                    .header("content-type", "application/json")
                    .header("originator", "fabro")
                    .json_body(serde_json::json!({
                        "device_auth_id": "deviceauth_123",
                        "user_code": "ABCD-EFGH"
                    }));
                then.status(403)
                    .header("content-type", "application/json")
                    .body(
                        serde_json::json!({
                            "error": {
                                "message": "authorization pending",
                                "code": "authorization_pending"
                            }
                        })
                        .to_string(),
                    );
            })
            .await;
        let success_poll_mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/api/accounts/deviceauth/token")
                    .header("content-type", "application/json")
                    .header("originator", "fabro")
                    .json_body(serde_json::json!({
                        "device_auth_id": "deviceauth_123",
                        "user_code": "ABCD-EFGH"
                    }));
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        serde_json::json!({
                            "authorization_code": "auth-code-123",
                            "code_verifier": "returned-verifier-456"
                        })
                        .to_string(),
                    );
            })
            .await;
        let token_mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .form_urlencoded_tuple("grant_type", "authorization_code")
                    .form_urlencoded_tuple("code", "auth-code-123")
                    .form_urlencoded_tuple(
                        "redirect_uri",
                        "https://auth.openai.com/deviceauth/callback",
                    )
                    .form_urlencoded_tuple("client_id", "test-client")
                    .form_urlencoded_tuple("code_verifier", "returned-verifier-456");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        serde_json::json!({
                            "access_token": "new-access-token",
                            "refresh_token": "new-refresh-token",
                            "expires_in": 3600,
                            "id_token": make_test_jwt(&json!({
                                "chatgpt_account_id": "acct_123"
                            }))
                        })
                        .to_string(),
                    );
            })
            .await;

        let mut strategy = CodexDeviceStrategy::new(test_config(&server));
        strategy.init().await.unwrap();

        let complete = tokio::spawn(async move {
            strategy
                .complete(AuthContextResponse::DeviceCodeConfirmed)
                .await
        });
        while pending_poll_mock.calls_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        assert!(pending_poll_mock.calls_async().await > 0);
        pending_poll_mock.delete_async().await;

        let credential = complete.await.unwrap().unwrap();

        let AuthDetails::CodexOAuth {
            tokens, account_id, ..
        } = credential.details
        else {
            panic!("expected codex oauth credential");
        };
        assert_eq!(tokens.access_token, "new-access-token");
        assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh-token"));
        assert_eq!(account_id.as_deref(), Some("acct_123"));
        init_mock.assert_async().await;
        success_poll_mock.assert_async().await;
        token_mock.assert_async().await;
    }
}
