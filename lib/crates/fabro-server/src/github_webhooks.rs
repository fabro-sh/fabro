use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::process::Command;
use tracing::{info, warn};

type HmacSha256 = Hmac<Sha256>;

/// Name of the server secret holding the GitHub App webhook HMAC key.
pub(crate) const WEBHOOK_SECRET_ENV: &str = "GITHUB_APP_WEBHOOK_SECRET";

/// Route path where Fabro receives GitHub App webhook deliveries.
pub(crate) const WEBHOOK_ROUTE: &str = "/api/v1/webhooks/github";

/// Verify a GitHub webhook HMAC-SHA256 signature.
///
/// `signature_header` is the value of the `X-Hub-Signature-256` header,
/// expected in the form `sha256=<hex-digest>`.
pub(crate) fn verify_signature(secret: &[u8], body: &[u8], signature_header: &str) -> bool {
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

pub(crate) fn parse_event_metadata(body: &[u8]) -> (String, String) {
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

/// Manages `tailscale funnel` lifecycle for the main Fabro server port.
pub struct TailscaleFunnelManager {
    port: u16,
}

impl TailscaleFunnelManager {
    pub async fn start(
        main_server_port: u16,
        app_id: &str,
        private_key_pem: &str,
    ) -> anyhow::Result<Self> {
        let funnel_url = enable_tailscale_funnel(main_server_port).await?;
        info!(port = main_server_port, url = %funnel_url, "Tailscale funnel enabled");

        let webhook_url = format!("{funnel_url}{WEBHOOK_ROUTE}");
        match update_github_app_webhook(app_id, private_key_pem, &webhook_url).await {
            Ok(()) => {
                info!(url = %webhook_url, "GitHub App webhook URL updated");
            }
            Err(err) => {
                warn!(
                    error = %err,
                    url = %webhook_url,
                    "Failed to update GitHub App webhook URL"
                );
            }
        }

        Ok(Self {
            port: main_server_port,
        })
    }

    pub async fn shutdown(self) {
        disable_tailscale_funnel(self.port).await;
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

    let status_output = Command::new("tailscale")
        .args(["funnel", "status"])
        .output()
        .await?;
    if !status_output.status.success() {
        let stderr = String::from_utf8_lossy(&status_output.stderr);
        anyhow::bail!("tailscale funnel status failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&status_output.stdout);
    parse_tailscale_funnel_url(&stdout)
        .ok_or_else(|| anyhow::anyhow!("Could not parse funnel URL from: {stdout}"))
}

fn parse_tailscale_funnel_url(status_output: &str) -> Option<String> {
    status_output.lines().find_map(|line| {
        line.split_whitespace()
            .find(|part| part.starts_with("https://"))
            .map(|url| url.trim_end_matches('/').to_string())
    })
}

async fn disable_tailscale_funnel(port: u16) {
    match Command::new("tailscale")
        .args(["funnel", "off", &port.to_string()])
        .output()
        .await
    {
        Ok(output) if output.status.success() => {
            info!(port, "Tailscale funnel disabled");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(port, error = %stderr, "Failed to disable Tailscale funnel");
        }
        Err(err) => {
            warn!(port, error = %err, "Failed to disable Tailscale funnel");
        }
    }
}

pub(crate) async fn update_github_app_webhook(
    app_id: &str,
    private_key_pem: &str,
    webhook_url: &str,
) -> anyhow::Result<()> {
    let jwt =
        fabro_github::sign_app_jwt(app_id, private_key_pem).map_err(|e| anyhow::anyhow!(e))?;

    let client = fabro_http::http_client()?;
    let body = serde_json::json!({
        "url": webhook_url,
        "content_type": "json",
    });

    let url = format!("{}/app/hook/config", fabro_github::github_api_base_url());
    let resp = client
        .patch(&url)
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
pub(crate) fn compute_signature(secret: &[u8], body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).unwrap();
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn parse_event_metadata_defaults_missing_fields() {
        assert_eq!(
            parse_event_metadata(br#"{"repository":{}}"#),
            ("unknown".to_string(), "none".to_string(),)
        );
    }

    #[test]
    fn parse_tailscale_funnel_url_extracts_https_origin() {
        let status = "https://fabro.example.ts.net proxy http://127.0.0.1:32276";

        assert_eq!(
            parse_tailscale_funnel_url(status),
            Some("https://fabro.example.ts.net".to_string())
        );
    }
}
