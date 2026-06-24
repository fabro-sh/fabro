use std::collections::HashMap;

use async_trait::async_trait;
use bollard::Docker;
use bollard::container::{InspectContainerOptions, ListContainersOptions, RemoveContainerOptions};
use bollard::errors::Error as DockerError;
use bollard::models::ContainerInspectResponse;
use fabro_types::{SandboxInfo, SandboxProviderKind};
use futures::future::try_join_all;

use super::{SandboxCreateSpec, SandboxProvider};
use crate::docker::DockerSandbox;
use crate::managed_labels::{self, MANAGED_LABEL, MANAGED_LABEL_VALUE};
use crate::{Sandbox, details};

#[derive(Debug, Clone, Default)]
pub struct DockerSandboxProvider;

impl DockerSandboxProvider {
    pub fn new() -> Self {
        Self
    }

    pub async fn check_daemon() -> crate::Result<()> {
        let docker = Self::docker_client()?;
        docker
            .ping()
            .await
            .map_err(|err| crate::Error::context("Failed to reach Docker daemon", err))?;
        Ok(())
    }

    fn docker_client() -> crate::Result<Docker> {
        Docker::connect_with_local_defaults().map_err(crate::Error::docker_connect)
    }
}

#[async_trait]
impl SandboxProvider for DockerSandboxProvider {
    fn kind(&self) -> SandboxProviderKind {
        SandboxProviderKind::Docker
    }

    async fn list(&self) -> crate::Result<Vec<SandboxInfo>> {
        let docker = Self::docker_client()?;
        let mut filters = HashMap::new();
        filters.insert("label".to_string(), vec![format!(
            "{MANAGED_LABEL}={MANAGED_LABEL_VALUE}"
        )]);
        let options = ListContainersOptions::<String> {
            all: true,
            filters,
            ..Default::default()
        };
        let containers = docker
            .list_containers(Some(options))
            .await
            .map_err(|err| crate::Error::context("Failed to list Docker containers", err))?;

        let ids: Vec<String> = containers.into_iter().filter_map(|c| c.id).collect();
        // Daemon-side label filter already restricts to managed containers, so we
        // can skip the per-inspect managed re-check. Run inspects concurrently on
        // the shared Docker client to avoid a serial N+1 round-trip.
        let inspects = try_join_all(
            ids.iter()
                .map(|id| docker.inspect_container(id, None::<InspectContainerOptions>)),
        )
        .await
        .map_err(|err| crate::Error::context("Failed to inspect Docker container", err))?;
        Ok(inspects
            .iter()
            .map(details::docker::docker_info_from_inspect)
            .collect())
    }

    async fn get(&self, id: &str) -> crate::Result<Option<SandboxInfo>> {
        let docker = Self::docker_client()?;
        let Some(inspect) = inspect_container(&docker, id).await? else {
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
        let docker = Self::docker_client()?;
        let Some(inspect) = inspect_container(&docker, id).await? else {
            return Ok(());
        };
        if !managed_from_inspect(&inspect) {
            return Err(crate::Error::message(format!(
                "Refusing to delete Docker container '{id}' because it is missing label {MANAGED_LABEL}={MANAGED_LABEL_VALUE}"
            )));
        }

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

async fn inspect_container(
    docker: &Docker,
    id: &str,
) -> crate::Result<Option<ContainerInspectResponse>> {
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

fn managed_from_inspect(inspect: &ContainerInspectResponse) -> bool {
    inspect
        .config
        .as_ref()
        .and_then(|config| config.labels.as_ref())
        .is_some_and(managed_labels::is_managed)
}

fn docker_not_found(error: &DockerError) -> bool {
    matches!(error, DockerError::DockerResponseServerError {
        status_code: 404,
        ..
    })
}
