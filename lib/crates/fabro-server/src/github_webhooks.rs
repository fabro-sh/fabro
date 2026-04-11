use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::oneshot;
use tracing::{debug, error, info, warn};

type HmacSha256 = Hmac<Sha256>;

/// Verify a GitHub webhook HMAC-SHA256 signature.
///
/// `signature_header` is the value of the `X-Hub-Signature-256` header,
/// expected in the form `sha256=<hex-digest>`.
pub fn verify_signature(secret: &[u8], body: &[u8], signature_header: &str) -> bool {
    let Some(hex_digest) = signature_header.strip_prefix("sha256=") else {
        return false;
    };

    let Ok(expected) = hex::decode(hex_digest) else {
        return false;
    };

    let Ok(mut mac) = HmacSha256::new_from_slice(secret) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

#[derive(Clone)]
struct WebhookState {
    secret: Vec<u8>,
}

async fn webhook_handler(
    State(state): State<WebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let delivery_id = headers
        .get("x-github-delivery")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    let Some(signature) = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
    else {
        warn!(delivery = %delivery_id, "Webhook signature verification failed");
        return StatusCode::UNAUTHORIZED;
    };

    if !verify_signature(&state.secret, &body, signature) {
        warn!(delivery = %delivery_id, "Webhook signature verification failed");
        return StatusCode::UNAUTHORIZED;
    }

    let event_type = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    if tracing::enabled!(tracing::Level::DEBUG) {
        let (repo, action) = parse_event_metadata(&body);
        debug!(
            event = %event_type,
            delivery = %delivery_id,
            repo = %repo,
            action = %action,
            "Webhook received"
        );
    } else {
        info!(
            event = %event_type,
            delivery = %delivery_id,
            "Webhook received"
        );
    }

    StatusCode::OK
}

fn parse_event_metadata(body: &[u8]) -> (String, String) {
    let parsed: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let repo = parsed
        .get("repository")
        .and_then(|r| r.get("full_name"))
        .and_then(|n| n.as_str())
        .unwrap_or("unknown")
        .to_string();
    let action = parsed
        .get("action")
        .and_then(|a| a.as_str())
        .unwrap_or("none")
        .to_string();
    (repo, action)
}

/// A running webhook listener that can be shut down.
pub struct WebhookListener {
    port: u16,
    shutdown_tx: oneshot::Sender<()>,
}

impl WebhookListener {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        info!("Webhook listener stopped");
    }
}

/// Spawn the webhook HTTP listener on a random port (127.0.0.1 only).
pub async fn spawn_webhook_listener(secret: Vec<u8>) -> anyhow::Result<WebhookListener> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    let state = WebhookState { secret };
    let router = Router::new()
        .route("/webhooks/github", post(webhook_handler))
        .with_state(state);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .ok();
    });

    info!(port = port, "Webhook listener started");

    Ok(WebhookListener { port, shutdown_tx })
}

/// Manage the full webhook lifecycle: listener + tailscale funnel + GitHub API.
pub struct WebhookManager {
    listener: WebhookListener,
}

impl WebhookManager {
    /// Start the webhook system: spawn listener, enable Tailscale funnel,
    /// and update the GitHub App webhook URL.
    pub async fn start(
        secret: Vec<u8>,
        app_id: &str,
        private_key_pem: &str,
    ) -> anyhow::Result<Self> {
        let listener = spawn_webhook_listener(secret).await?;
        let port = listener.port();

        // Enable Tailscale funnel
        let funnel_url = match enable_tailscale_funnel(port).await {
            Ok(url) => url,
            Err(err) => {
                error!(error = %err, "Failed to enable Tailscale funnel");
                listener.shutdown();
                return Err(err);
            }
        };

        info!(url = %funnel_url, "Tailscale funnel enabled");

        // Update GitHub App webhook URL
        let webhook_url = format!("{funnel_url}/webhooks/github");
        if let Err(err) = update_github_app_webhook(app_id, private_key_pem, &webhook_url).await {
            error!(error = %err, "Failed to update GitHub App webhook URL");
            disable_tailscale_funnel(port).await;
            listener.shutdown();
            return Err(err);
        }

        info!(url = %webhook_url, "GitHub App webhook URL updated");

        Ok(Self { listener })
    }

    /// Shut down: disable funnel, stop listener.
    pub async fn shutdown(self) {
        disable_tailscale_funnel(self.listener.port()).await;
        self.listener.shutdown();
    }
}

async fn enable_tailscale_funnel(port: u16) -> anyhow::Result<String> {
    let output = Command::new("tailscale")
        .args(["funnel", &port.to_string()])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tailscale funnel failed: {stderr}");
    }

    // Get the funnel URL from `tailscale funnel status`
    let status_output = Command::new("tailscale")
        .args(["funnel", "status"])
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&status_output.stdout);
    // Parse the HTTPS URL from status output — first line typically contains it
    let url = stdout
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with("https://") {
                // Strip trailing path/colon info
                Some(trimmed.trim_end_matches('/').to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| anyhow::anyhow!("Could not parse funnel URL from: {stdout}"))?;

    Ok(url)
}

async fn disable_tailscale_funnel(port: u16) {
    match Command::new("tailscale")
        .args(["funnel", "off", &port.to_string()])
        .output()
        .await
    {
        Ok(output) if output.status.success() => {
            info!("Tailscale funnel disabled");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(error = %stderr, "Failed to disable Tailscale funnel");
        }
        Err(err) => {
            warn!(error = %err, "Failed to disable Tailscale funnel");
        }
    }
}

async fn update_github_app_webhook(
    app_id: &str,
    private_key_pem: &str,
    webhook_url: &str,
) -> anyhow::Result<()> {
    let jwt =
        fabro_github::sign_app_jwt(app_id, private_key_pem).map_err(|e| anyhow::anyhow!(e))?;

    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "url": webhook_url,
        "content_type": "json",
    });

    let resp = client
        .patch("https://api.github.com/app/hook/config")
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro")
        .json(&body)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("GitHub API returned {status}: {text}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;

    fn test_http_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    // -----------------------------------------------------------------------
    // verify_signature
    // -----------------------------------------------------------------------

    fn compute_signature(secret: &[u8], body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(body);
        let result = mac.finalize();
        format!("sha256={}", hex::encode(result.into_bytes()))
    }

    #[test]
    fn valid_signature() {
        let secret = b"test-secret";
        let body = b"hello world";
        let sig = compute_signature(secret, body);
        assert!(verify_signature(secret, body, &sig));
    }

    #[test]
    fn wrong_signature() {
        let secret = b"test-secret";
        let body = b"hello world";
        let sig = compute_signature(b"wrong-secret", body);
        assert!(!verify_signature(secret, body, &sig));
    }

    #[test]
    fn missing_sha256_prefix() {
        let secret = b"test-secret";
        let body = b"hello world";
        let mut sig = compute_signature(secret, body);
        sig = sig.replace("sha256=", "");
        assert!(!verify_signature(secret, body, &sig));
    }

    #[test]
    fn empty_body_valid_signature() {
        let secret = b"test-secret";
        let body = b"";
        let sig = compute_signature(secret, body);
        assert!(verify_signature(secret, body, &sig));
    }

    // -----------------------------------------------------------------------
    // webhook_handler
    // -----------------------------------------------------------------------

    fn build_test_router(secret: &[u8]) -> Router {
        let state = WebhookState {
            secret: secret.to_vec(),
        };
        Router::new()
            .route("/webhooks/github", post(webhook_handler))
            .with_state(state)
    }

    #[tokio::test]
    async fn rejects_missing_signature() {
        let app = build_test_router(b"secret");
        let req = Request::builder()
            .method("POST")
            .uri("/webhooks/github")
            .body(Body::from("{}"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_bad_signature() {
        let app = build_test_router(b"secret");
        let body = b"{}";
        let bad_sig = compute_signature(b"wrong", body);

        let req = Request::builder()
            .method("POST")
            .uri("/webhooks/github")
            .header("x-hub-signature-256", bad_sig)
            .body(Body::from(body.to_vec()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn accepts_valid_webhook() {
        let secret = b"my-secret";
        let app = build_test_router(secret);
        let body = br#"{"repository":{"full_name":"owner/repo"},"action":"opened"}"#;
        let sig = compute_signature(secret, body);

        let req = Request::builder()
            .method("POST")
            .uri("/webhooks/github")
            .header("x-hub-signature-256", sig)
            .header("x-github-event", "pull_request")
            .header("x-github-delivery", "abc-123")
            .body(Body::from(body.to_vec()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // -----------------------------------------------------------------------
    // spawn_webhook_listener
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn spawn_listener_serves_route() {
        let secret = b"integration-secret";
        let listener = spawn_webhook_listener(secret.to_vec()).await.unwrap();
        let port = listener.port();

        // Valid request should return 200
        let body = b"{}";
        let sig = compute_signature(secret, body);

        let client = test_http_client();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/webhooks/github"))
            .header("x-hub-signature-256", sig)
            .body(body.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Missing signature should return 401
        let resp = client
            .post(format!("http://127.0.0.1:{port}/webhooks/github"))
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);

        listener.shutdown();
    }
}
