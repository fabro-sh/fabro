use std::time::Duration;

use async_trait::async_trait;
use fabro_types::{SandboxInfo, SandboxProviderKind};
use tokio::time::sleep;

use super::{SandboxCreateSpec, SandboxProvider};
use crate::forkd::{ForkdConfig, ForkdSandbox};
use crate::{Sandbox, details};

/// Retry limit for transient HTTP failures (5xx / connect) in provider calls.
const PROVIDER_RETRY_LIMIT: u32 = 3;
const PROVIDER_RETRY_INITIAL_BACKOFF: Duration = Duration::from_millis(250);

/// A [`SandboxProvider`] that creates Firecracker microVMs via a forkd
/// controller.  The controller URL and bearer token are resolved once at
/// construction time from `FORKD_URL` / `FORKD_TOKEN`; they never appear in
/// per-run specs.
#[derive(Clone)]
pub struct ForkdSandboxProvider {
    config: ForkdConfig,
}

impl ForkdSandboxProvider {
    /// Build a provider from an already-resolved [`ForkdConfig`].
    pub fn new(config: ForkdConfig) -> Self {
        Self { config }
    }

    /// Build a provider by reading `FORKD_URL`, `FORKD_TOKEN`, and
    /// `FORKD_SNAPSHOT_TAG` from the process environment (the same
    /// resolution that [`ForkdConfig::from_env`] and
    /// [`crate::from_environment::forkd_config_from_environment`] perform).
    pub fn from_env() -> Self {
        Self::new(ForkdConfig::from_env())
    }

    fn http_client() -> crate::Result<reqwest::Client> {
        fabro_http::HttpClientBuilder::new()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| crate::Error::context("Failed to build HTTP client for forkd", e))
    }

    fn is_retryable_status(status: reqwest::StatusCode) -> bool {
        status.is_server_error()
    }
}

#[async_trait]
impl SandboxProvider for ForkdSandboxProvider {
    fn kind(&self) -> SandboxProviderKind {
        SandboxProviderKind::Forkd
    }

    async fn list(&self) -> crate::Result<Vec<SandboxInfo>> {
        let client = Self::http_client()?;
        let url = format!("{}/v1/sandboxes", self.config.forkd_url);

        let mut backoff = PROVIDER_RETRY_INITIAL_BACKOFF;
        let mut attempt = 0u32;
        let resp = loop {
            let result = client
                .get(&url)
                .bearer_auth(&self.config.forkd_token)
                .send()
                .await;

            match result {
                Ok(resp) if resp.status().is_success() => break resp,
                Ok(resp)
                    if Self::is_retryable_status(resp.status())
                        && attempt < PROVIDER_RETRY_LIMIT =>
                {
                    let status = resp.status();
                    tracing::warn!(
                        attempt,
                        status = status.as_u16(),
                        "forkd list transient error; retrying"
                    );
                    attempt += 1;
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(crate::Error::message(format!(
                        "forkd list VMs returned {status}: {body}"
                    )));
                }
                Err(e) if e.is_connect() && attempt < PROVIDER_RETRY_LIMIT => {
                    tracing::warn!(attempt, error = %e, "forkd list connect error; retrying");
                    attempt += 1;
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
                Err(e) => {
                    return Err(crate::Error::context("Failed to list forkd VMs", e));
                }
            }
        };

        let vms: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| crate::Error::context("Failed to parse forkd VM list", e))?;

        // forkd 0.5.2 returns a top-level JSON array of sandbox objects, each
        // carrying an "id" field.
        let arr = vms.as_array().cloned().unwrap_or_default();

        let sandboxes = arr
            .into_iter()
            .filter_map(|vm| {
                let id = vm.get("id")?.as_str()?.to_string();
                Some(details::forkd::forkd_info_from_name(&id))
            })
            .collect();

        Ok(sandboxes)
    }

    async fn get(&self, id: &str) -> crate::Result<Option<SandboxInfo>> {
        let client = Self::http_client()?;
        let url = format!("{}/v1/sandboxes/{}", self.config.forkd_url, id);

        let mut backoff = PROVIDER_RETRY_INITIAL_BACKOFF;
        let mut attempt = 0u32;
        loop {
            let result = client
                .get(&url)
                .bearer_auth(&self.config.forkd_token)
                .send()
                .await;

            match result {
                Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => return Ok(None),
                Ok(resp) if resp.status().is_success() => {
                    return Ok(Some(details::forkd::forkd_info_from_name(id)));
                }
                Ok(resp)
                    if Self::is_retryable_status(resp.status())
                        && attempt < PROVIDER_RETRY_LIMIT =>
                {
                    let status = resp.status();
                    tracing::warn!(
                        attempt,
                        status = status.as_u16(),
                        "forkd get transient error; retrying"
                    );
                    attempt += 1;
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(crate::Error::message(format!(
                        "forkd get VM '{id}' returned {status}: {body}"
                    )));
                }
                Err(e) if e.is_connect() && attempt < PROVIDER_RETRY_LIMIT => {
                    tracing::warn!(attempt, error = %e, "forkd get connect error; retrying");
                    attempt += 1;
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
                Err(e) => {
                    return Err(crate::Error::context(
                        format!("Failed to get forkd VM '{id}'"),
                        e,
                    ));
                }
            }
        }
    }

    async fn create(&self, spec: SandboxCreateSpec) -> crate::Result<SandboxInfo> {
        let SandboxCreateSpec::Forkd {
            config,
            run_id,
            clone_origin_url,
            clone_branch,
        } = spec
        else {
            return Err(crate::Error::message(
                "ForkdSandboxProvider requires a SandboxCreateSpec::Forkd variant",
            ));
        };

        let merged_config = ForkdConfig {
            forkd_url:   self.config.forkd_url.clone(),
            forkd_token: self.config.forkd_token.clone(),
            settings:    *config,
        };

        let sandbox = ForkdSandbox::new(merged_config, run_id, clone_origin_url, clone_branch);
        // Provision the microVM first; forkd assigns the sandbox id, which we
        // then report. Without initialize() the VM never exists and all
        // subsequent operations on the returned SandboxInfo would fail.
        sandbox.initialize().await?;
        let id = sandbox
            .sandbox_id()
            .ok_or_else(|| crate::Error::message("forkd sandbox id missing after initialize"))?;
        Ok(details::forkd::forkd_info_from_name(id))
    }

    async fn delete(&self, id: &str) -> crate::Result<()> {
        let client = Self::http_client()?;
        let url = format!("{}/v1/sandboxes/{}", self.config.forkd_url, id);

        let mut backoff = PROVIDER_RETRY_INITIAL_BACKOFF;
        let mut attempt = 0u32;
        loop {
            let result = client
                .delete(&url)
                .bearer_auth(&self.config.forkd_token)
                .send()
                .await;

            match result {
                Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                    // Already gone — treat as success.
                    return Ok(());
                }
                Ok(resp) if resp.status().is_success() => return Ok(()),
                Ok(resp)
                    if Self::is_retryable_status(resp.status())
                        && attempt < PROVIDER_RETRY_LIMIT =>
                {
                    let status = resp.status();
                    tracing::warn!(
                        attempt,
                        status = status.as_u16(),
                        "forkd delete transient error; retrying"
                    );
                    attempt += 1;
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(crate::Error::message(format!(
                        "forkd delete VM '{id}' returned {status}: {body}"
                    )));
                }
                Err(e) if e.is_connect() && attempt < PROVIDER_RETRY_LIMIT => {
                    tracing::warn!(attempt, error = %e, "forkd delete connect error; retrying");
                    attempt += 1;
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
                Err(e) => {
                    return Err(crate::Error::context(
                        format!("Failed to delete forkd VM '{id}'"),
                        e,
                    ));
                }
            }
        }
    }
}
