use std::collections::HashMap;

use async_trait::async_trait;
use daytona_sdk::DaytonaError;
use fabro_static::EnvVars;
use fabro_types::{SandboxInfo, SandboxProviderKind};

use super::{SandboxCreateSpec, SandboxProvider};
use crate::daytona::{self, DaytonaSandbox};
use crate::managed_labels::{self, MANAGED_LABEL, MANAGED_LABEL_VALUE};
use crate::{Sandbox, details};

const DAYTONA_LIST_PAGE_SIZE: i32 = 100;

#[derive(Clone)]
pub struct DaytonaSandboxProvider {
    api_key:         Option<String>,
    api_url:         Option<String>,
    organization_id: Option<String>,
    http_client:     Option<fabro_http::HttpClient>,
}

impl DaytonaSandboxProvider {
    pub fn new(
        api_key: Option<String>,
        api_url: Option<String>,
        organization_id: Option<String>,
        http_client: Option<fabro_http::HttpClient>,
    ) -> Self {
        Self {
            api_key,
            api_url,
            organization_id,
            http_client,
        }
    }

    async fn client(&self) -> crate::Result<daytona_sdk::Client> {
        let api_key = self.api_key.clone().ok_or_else(|| {
            crate::Error::message(format!("{} is not configured", EnvVars::DAYTONA_API_KEY))
        })?;
        daytona::build_daytona_client_with(
            Some(api_key),
            self.api_url.clone(),
            self.organization_id.clone(),
            self.http_client.clone(),
        )
        .await
        .map_err(|err| crate::Error::context("Failed to create Daytona client", err))
    }
}

#[async_trait]
impl SandboxProvider for DaytonaSandboxProvider {
    fn kind(&self) -> SandboxProviderKind {
        SandboxProviderKind::Daytona
    }

    async fn list(&self) -> crate::Result<Vec<SandboxInfo>> {
        let client = self.client().await?;
        let labels = HashMap::from([(MANAGED_LABEL.to_string(), MANAGED_LABEL_VALUE.to_string())]);
        let mut page = 1;
        let mut sandboxes = Vec::new();

        loop {
            let result = client
                .list(Some(&labels), Some(page), Some(DAYTONA_LIST_PAGE_SIZE))
                .await
                .map_err(|err| crate::Error::context("Failed to list Daytona sandboxes", err))?;
            // The Daytona API already filters by the managed label above; map every
            // returned sandbox without re-checking the label client-side.
            sandboxes.extend(
                result
                    .items
                    .iter()
                    .map(details::daytona::daytona_info_from_sdk_sandbox),
            );

            if result.total_pages <= i64::from(page) {
                break;
            }
            page += 1;
        }

        Ok(sandboxes)
    }

    async fn get(&self, id: &str) -> crate::Result<Option<SandboxInfo>> {
        let client = self.client().await?;
        let sandbox = match client.get(id).await {
            Ok(sandbox) => sandbox,
            Err(err) if daytona_not_found(&err) => return Ok(None),
            Err(err) => {
                return Err(crate::Error::context(
                    format!("Failed to get Daytona sandbox '{id}'"),
                    err,
                ));
            }
        };

        if !managed_from_sdk_sandbox(&sandbox) {
            return Ok(None);
        }
        Ok(Some(details::daytona::daytona_info_from_sdk_sandbox(
            &sandbox,
        )))
    }

    async fn create(&self, spec: SandboxCreateSpec) -> crate::Result<SandboxInfo> {
        let SandboxCreateSpec::Daytona {
            config,
            github_app,
            run_id,
            clone_origin_url,
            clone_branch,
            api_key,
        } = spec
        else {
            return Err(crate::Error::message(
                "Daytona sandbox provider can only create Daytona sandboxes",
            ));
        };

        let api_key = api_key.or_else(|| self.api_key.clone()).ok_or_else(|| {
            crate::Error::message(format!("{} is not configured", EnvVars::DAYTONA_API_KEY))
        })?;
        let sandbox = DaytonaSandbox::new(
            config.as_ref().clone(),
            github_app,
            run_id,
            clone_origin_url,
            clone_branch,
            Some(api_key),
        )
        .await?;
        sandbox.initialize().await?;
        let sdk_sandbox = sandbox.sandbox_handle().ok_or_else(|| {
            crate::Error::message("Daytona sandbox was created but no SDK handle is available")
        })?;
        Ok(details::daytona::daytona_info_from_sdk_sandbox(sdk_sandbox))
    }

    async fn delete(&self, id: &str) -> crate::Result<()> {
        let client = self.client().await?;
        let sandbox = match client.get(id).await {
            Ok(sandbox) => sandbox,
            Err(err) if daytona_not_found(&err) => return Ok(()),
            Err(err) => {
                return Err(crate::Error::context(
                    format!("Failed to get Daytona sandbox '{id}' before delete"),
                    err,
                ));
            }
        };
        if !managed_from_sdk_sandbox(&sandbox) {
            return Err(crate::Error::message(format!(
                "Refusing to delete Daytona sandbox '{id}' because it is missing label {MANAGED_LABEL}={MANAGED_LABEL_VALUE}"
            )));
        }
        client.delete(&sandbox.id).await.map_err(|err| {
            crate::Error::context(format!("Failed to delete Daytona sandbox '{id}'"), err)
        })
    }
}

fn managed_from_sdk_sandbox(sandbox: &daytona_sdk::Sandbox) -> bool {
    managed_labels::is_managed(&sandbox.labels)
}

fn daytona_not_found(err: &DaytonaError) -> bool {
    matches!(err, DaytonaError::NotFound { .. }) || err.status_code() == Some(404)
}
