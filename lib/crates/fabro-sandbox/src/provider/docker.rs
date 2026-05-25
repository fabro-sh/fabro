use std::collections::HashMap;

use async_trait::async_trait;
use bollard::Docker;
use bollard::container::{InspectContainerOptions, ListContainersOptions, RemoveContainerOptions};
use bollard::errors::Error as DockerError;
use bollard::models::ContainerInspectResponse;
use fabro_types::{SandboxInfo, SandboxProviderKind};

use super::{SandboxCreateSpec, SandboxListFilter, SandboxProvider};
use crate::docker::{DockerSandbox, DockerSandboxOptions};
use crate::managed_labels::MANAGED_LABEL;
use crate::{Sandbox, details};

#[derive(Debug, Clone)]
pub struct DockerSandboxProvider {
    _default_config: DockerSandboxOptions,
}

impl DockerSandboxProvider {
    pub fn new(default_config: DockerSandboxOptions) -> Self {
        Self {
            _default_config: default_config,
        }
    }

    fn docker_client() -> crate::Result<Docker> {
        Docker::connect_with_local_defaults().map_err(crate::Error::docker_connect)
    }

    async fn inspect(&self, id: &str) -> crate::Result<Option<ContainerInspectResponse>> {
        let docker = Self::docker_client()?;
        match docker
            .inspect_container(id, None::<InspectContainerOptions>)
            .await
        {
            Ok(inspect) => Ok(Some(inspect)),
            Err(err) if docker_not_found(&err) => Ok(None),
            Err(err) => Err(crate::Error::context(
                format!("Failed to inspect Docker container '{id}'"),
                err,
            )),
        }
    }
}

impl Default for DockerSandboxProvider {
    fn default() -> Self {
        Self::new(DockerSandboxOptions::default())
    }
}

#[async_trait]
impl SandboxProvider for DockerSandboxProvider {
    fn kind(&self) -> SandboxProviderKind {
        SandboxProviderKind::Docker
    }

    async fn list(&self, _filter: SandboxListFilter) -> crate::Result<Vec<SandboxInfo>> {
        let docker = Self::docker_client()?;
        let mut filters = HashMap::new();
        filters.insert("label".to_string(), vec![format!("{MANAGED_LABEL}=true")]);
        let options = ListContainersOptions::<String> {
            all: true,
            filters,
            ..Default::default()
        };
        let containers = docker
            .list_containers(Some(options))
            .await
            .map_err(|err| crate::Error::context("Failed to list Docker containers", err))?;

        let mut sandboxes = Vec::new();
        for container in containers {
            let Some(id) = container.id else {
                continue;
            };
            let Some(inspect) = self.inspect(&id).await? else {
                continue;
            };
            if managed_from_inspect(&inspect) {
                sandboxes.push(details::docker::docker_info_from_inspect(&inspect));
            }
        }
        Ok(sandboxes)
    }

    async fn get(&self, id: &str) -> crate::Result<Option<SandboxInfo>> {
        let Some(inspect) = self.inspect(id).await? else {
            return Ok(None);
        };
        if !managed_from_inspect(&inspect) {
            return Ok(None);
        }
        Ok(Some(details::docker::docker_info_from_inspect(&inspect)))
    }

    async fn create(&self, spec: SandboxCreateSpec) -> crate::Result<SandboxInfo> {
        let SandboxCreateSpec::Docker {
            config,
            github_app,
            run_id,
            clone_origin_url,
            clone_branch,
        } = spec
        else {
            return Err(crate::Error::message(
                "Docker sandbox provider can only create Docker sandboxes",
            ));
        };

        let sandbox =
            DockerSandbox::new(config, github_app, run_id, clone_origin_url, clone_branch)?;
        sandbox.initialize().await?;
        let container_id = sandbox.container_identifier()?.to_string();
        self.get(&container_id).await?.ok_or_else(|| {
            crate::Error::message(format!(
                "Docker sandbox '{container_id}' was created but is not visible in provider inventory"
            ))
        })
    }

    async fn delete(&self, id: &str) -> crate::Result<()> {
        let Some(inspect) = self.inspect(id).await? else {
            return Ok(());
        };
        if !managed_from_inspect(&inspect) {
            return Err(crate::Error::message(format!(
                "Refusing to delete Docker container '{id}' because it is missing label {MANAGED_LABEL}=true"
            )));
        }

        let docker = Self::docker_client()?;
        let container_id = inspect.id.as_deref().unwrap_or(id);
        docker
            .remove_container(
                container_id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|err| {
                crate::Error::context(
                    format!("Failed to remove Docker container '{container_id}'"),
                    err,
                )
            })
    }
}

fn managed_from_inspect(inspect: &ContainerInspectResponse) -> bool {
    inspect
        .config
        .as_ref()
        .and_then(|config| config.labels.as_ref())
        .and_then(|labels| labels.get(MANAGED_LABEL))
        .map(String::as_str)
        == Some("true")
}

fn docker_not_found(error: &DockerError) -> bool {
    matches!(error, DockerError::DockerResponseServerError {
        status_code: 404,
        ..
    })
}
