//! [`SandboxProvider`] surface for the gcloud (GCE-per-run) backend.
//!
//! Mirrors the Daytona provider's shape: list/get/delete filter strictly on
//! the managed label and **refuse to touch unmanaged resources**; create builds
//! a [`GcloudSandbox`], initializes it (insert VM → pin host key → SSH → clone)
//! and projects the live instance into a [`SandboxInfo`].

use std::sync::Arc;

use async_trait::async_trait;
use fabro_types::{SandboxInfo, SandboxProviderKind};

use super::{SandboxCreateSpec, SandboxProvider};
use crate::Sandbox;
use crate::gcloud::compute::ComputeClient;
use crate::gcloud::config::{GcloudConfig, GcloudSettings};
use crate::gcloud::{GcloudSandbox, auth::GcpAuth};

/// GCE-per-run provider. Stateless beyond the resolved operator settings, a
/// shared HTTP client, and a shared credential source.
#[derive(Clone)]
pub struct GcloudSandboxProvider {
    settings: GcloudSettings,
    http: reqwest::Client,
    /// One shared [`GcpAuth`] so the access-token cache is process-wide: every
    /// `compute()` and each created [`GcloudSandbox`] reuse a single minted
    /// token across operations instead of re-minting (and re-signing an SA JWT /
    /// re-round-tripping the token endpoint) per list/get/create/delete.
    auth: Arc<GcpAuth>,
}

impl GcloudSandboxProvider {
    #[must_use]
    pub fn new(settings: GcloudSettings, http: reqwest::Client) -> Self {
        let auth = Arc::new(GcpAuth::new(http.clone(), settings.sa_key_json.clone()));
        Self {
            settings,
            http,
            auth,
        }
    }

    fn compute(&self) -> ComputeClient {
        ComputeClient::new(self.http.clone(), Arc::clone(&self.auth))
    }

    /// Resolve a config for read-only operations (list/get/delete), which only
    /// need substrate identity, not a per-run egress policy.
    fn read_config(&self) -> crate::Result<GcloudConfig> {
        GcloudConfig::resolve(&self.settings, crate::gcloud::EgressPolicy::AllowAll)
    }
}

#[async_trait]
impl SandboxProvider for GcloudSandboxProvider {
    fn kind(&self) -> SandboxProviderKind {
        SandboxProviderKind::Gcloud
    }

    async fn list(&self) -> crate::Result<Vec<SandboxInfo>> {
        let config = self.read_config()?;
        let instances = self.compute().list_managed(&config).await?;
        Ok(instances
            .iter()
            .map(|instance| crate::details::gcloud::gcloud_info_from_instance(instance, &config))
            .collect())
    }

    async fn get(&self, id: &str) -> crate::Result<Option<SandboxInfo>> {
        let config = self.read_config()?;
        match self.compute().get_instance(&config, id).await {
            Ok(instance) if instance.is_managed() => Ok(Some(
                crate::details::gcloud::gcloud_info_from_instance(&instance, &config),
            )),
            Ok(_) => Ok(None),
            Err(err) if is_not_found(&err) => Ok(None),
            Err(err) => Err(err),
        }
    }

    async fn create(&self, spec: SandboxCreateSpec) -> crate::Result<SandboxInfo> {
        let SandboxCreateSpec::Gcloud {
            config,
            github_app,
            run_id,
            clone_origin_url,
            clone_branch,
        } = spec
        else {
            return Err(crate::Error::message(
                "gcloud sandbox provider can only create gcloud sandboxes",
            ));
        };

        let read_config = (*config).clone();
        let sandbox = GcloudSandbox::new(
            *config,
            self.http.clone(),
            Arc::clone(&self.auth),
            run_id,
            clone_origin_url,
            clone_branch,
            github_app,
        )?;
        sandbox.initialize().await?;

        let name = sandbox.instance_name().ok_or_else(|| {
            crate::Error::message("gcloud sandbox initialized without an instance name")
        })?;
        let instance = self.compute().get_instance(&read_config, name).await?;
        Ok(crate::details::gcloud::gcloud_info_from_instance(
            &instance,
            &read_config,
        ))
    }

    async fn delete(&self, id: &str) -> crate::Result<()> {
        let config = self.read_config()?;
        let compute = self.compute();
        let instance = match compute.get_instance(&config, id).await {
            Ok(instance) => instance,
            Err(err) if is_not_found(&err) => return Ok(()),
            Err(err) => return Err(err),
        };
        if !instance.is_managed() {
            return Err(crate::Error::message(format!(
                "Refusing to delete GCE instance '{id}' because it is missing the managed label"
            )));
        }
        let operation = compute.delete_instance(&config, id).await?;
        compute.await_zonal_operation(&config, &operation).await
    }
}

fn is_not_found(err: &crate::Error) -> bool {
    err.to_string().contains("returned 404")
}
