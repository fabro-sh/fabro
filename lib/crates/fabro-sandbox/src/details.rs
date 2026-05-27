use std::collections::BTreeMap;

use anyhow::Result;
#[cfg(any(feature = "docker", feature = "daytona"))]
use chrono::{DateTime, Utc};
use fabro_types::{
    RunId, RunSandboxInstance, SandboxDetails, SandboxNetwork, SandboxProviderKind,
    SandboxResources, SandboxState, SandboxTimestamps,
};

/// Inspect the sandbox identified by `record` and return provider-neutral
/// details for control-plane display.
///
/// Provider feature flags determine which branches resolve real data:
/// - `local` always returns a minimal record describing the host.
/// - `docker` inspects the managed container through Bollard.
/// - `daytona` reconnects to the SDK sandbox.
#[allow(
    unused_variables,
    reason = "Feature-gated providers consume some parameters only when enabled."
)]
pub async fn sandbox_details(
    record: &RunSandboxInstance,
    daytona_api_key: Option<String>,
    daytona_organization_id: Option<String>,
    run_id: Option<RunId>,
) -> Result<SandboxDetails> {
    match record.provider {
        SandboxProviderKind::Local => Ok(local_details(record)),
        #[cfg(feature = "docker")]
        SandboxProviderKind::Docker => docker::docker_details(record, run_id).await,
        #[cfg(not(feature = "docker"))]
        SandboxProviderKind::Docker => Err(anyhow::anyhow!(
            "Sandbox provider '{}' has no details implementation",
            record.provider
        )),
        #[cfg(feature = "daytona")]
        SandboxProviderKind::Daytona => daytona::daytona_details(record, daytona_api_key).await,
        #[cfg(not(feature = "daytona"))]
        SandboxProviderKind::Daytona => Err(anyhow::anyhow!(
            "Sandbox provider '{}' has no details implementation",
            record.provider
        )),
    }
}

fn local_details(record: &RunSandboxInstance) -> SandboxDetails {
    SandboxDetails {
        sandbox:      record.clone(),
        state:        SandboxState::Running,
        native_state: None,
        region:       None,
        web_url:      None,
        resources:    SandboxResources::default(),
        network:      SandboxNetwork::unknown(),
        labels:       BTreeMap::new(),
        timestamps:   SandboxTimestamps::default(),
    }
}

#[cfg(any(feature = "docker", feature = "daytona"))]
fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(feature = "docker")]
pub(crate) mod docker {
    use std::collections::BTreeMap;

    use anyhow::{Context, Result, anyhow};
    use bollard::Docker;
    use bollard::container::InspectContainerOptions;
    use bollard::models::{ContainerInspectResponse, ContainerStateStatusEnum, HostConfig};
    use fabro_types::{
        RunId, RunSandboxInstance, SandboxDetails, SandboxInfo, SandboxNetwork,
        SandboxNetworkPolicy, SandboxProviderKind, SandboxResources, SandboxState,
        SandboxTimestamps,
    };

    use super::parse_rfc3339_utc;
    use crate::docker::WORKING_DIRECTORY;

    pub(super) async fn docker_details(
        record: &RunSandboxInstance,
        _run_id: Option<RunId>,
    ) -> Result<SandboxDetails> {
        let docker =
            Docker::connect_with_local_defaults().context("Failed to connect to Docker daemon")?;
        let runtime = &record.runtime;
        let inspect = docker
            .inspect_container(&runtime.id, None::<InspectContainerOptions>)
            .await
            .map_err(|err| anyhow!("Failed to inspect Docker container '{}': {err}", runtime.id))?;
        Ok(map_docker_inspect(&inspect, record))
    }

    pub(crate) fn docker_info_from_inspect(inspect: &ContainerInspectResponse) -> SandboxInfo {
        let fields = docker_fields_from_inspect(inspect);
        SandboxInfo {
            provider:          SandboxProviderKind::Docker,
            id:                fields.id,
            display_name:      fields.display_name,
            state:             fields.state,
            native_state:      fields.native_state,
            image:             fields.image,
            snapshot:          None,
            region:            None,
            web_url:           None,
            working_directory: fields.working_directory,
            resources:         fields.resources,
            network:           fields.network,
            labels:            fields.labels,
            timestamps:        fields.timestamps,
        }
    }

    pub(super) fn map_docker_inspect(
        inspect: &ContainerInspectResponse,
        record: &RunSandboxInstance,
    ) -> SandboxDetails {
        let fields = docker_fields_from_inspect(inspect);
        let image = fields.image.clone().or_else(|| record.image.clone());

        SandboxDetails {
            sandbox:      RunSandboxInstance {
                image,
                ..record.clone()
            },
            state:        fields.state,
            native_state: fields.native_state,
            region:       None,
            web_url:      None,
            resources:    fields.resources,
            network:      fields.network,
            labels:       fields.labels,
            timestamps:   fields.timestamps,
        }
    }

    struct DockerFields {
        id:                String,
        display_name:      Option<String>,
        state:             SandboxState,
        native_state:      Option<String>,
        image:             Option<String>,
        working_directory: Option<String>,
        resources:         SandboxResources,
        network:           SandboxNetwork,
        labels:            BTreeMap<String, String>,
        timestamps:        SandboxTimestamps,
    }

    fn docker_fields_from_inspect(inspect: &ContainerInspectResponse) -> DockerFields {
        let status_enum = inspect
            .state
            .as_ref()
            .and_then(|state| state.status.as_ref())
            .copied();
        let normalized_state = status_enum.map_or(SandboxState::Unknown, normalize_docker_state);
        let native_state = status_enum
            .map(|status| status.to_string())
            .filter(|value| !value.is_empty());

        let host_config = inspect.host_config.as_ref();
        let resources = SandboxResources {
            cpu_cores:    host_config.and_then(docker_cpu_cores),
            memory_bytes: host_config
                .and_then(|host| host.memory)
                .filter(|bytes| *bytes > 0)
                .and_then(|bytes| u64::try_from(bytes).ok()),
            disk_bytes:   None,
        };
        let network = docker_network(host_config);

        let labels: BTreeMap<String, String> = inspect
            .config
            .as_ref()
            .and_then(|config| config.labels.clone())
            .map(|map| map.into_iter().collect())
            .unwrap_or_default();

        let image = inspect
            .config
            .as_ref()
            .and_then(|config| config.image.clone())
            .or_else(|| inspect.image.clone())
            .filter(|value| !value.is_empty());
        let working_directory = inspect
            .config
            .as_ref()
            .and_then(|config| config.working_dir.clone())
            .filter(|value| !value.is_empty())
            .or_else(|| Some(WORKING_DIRECTORY.to_string()));

        let id = inspect
            .id
            .clone()
            .or_else(|| inspect.name.as_ref().map(|name| trim_container_name(name)))
            .unwrap_or_default();
        let display_name = inspect
            .name
            .as_ref()
            .map(|name| trim_container_name(name))
            .filter(|name| !name.is_empty());

        let created_at = inspect.created.as_deref().and_then(parse_rfc3339_utc);

        DockerFields {
            id,
            display_name,
            state: normalized_state,
            native_state,
            image,
            working_directory,
            resources,
            network,
            labels,
            timestamps: SandboxTimestamps {
                created_at,
                last_activity_at: None,
            },
        }
    }

    fn trim_container_name(name: &str) -> String {
        name.strip_prefix('/').unwrap_or(name).to_string()
    }

    fn docker_network(host_config: Option<&HostConfig>) -> SandboxNetwork {
        match host_config.and_then(|host| host.network_mode.as_deref()) {
            Some("none") => {
                let blocked = SandboxNetworkPolicy::blocked();
                SandboxNetwork {
                    egress:  blocked.clone(),
                    ingress: blocked,
                }
            }
            _ => SandboxNetwork::unknown(),
        }
    }

    pub(super) fn docker_cpu_cores(host_config: &HostConfig) -> Option<f64> {
        let quota = host_config.cpu_quota?;
        let period = host_config.cpu_period?;
        if quota <= 0 || period <= 0 {
            return None;
        }
        #[allow(
            clippy::cast_precision_loss,
            reason = "CPU quota/period are bounded well within f64 mantissa precision."
        )]
        let cores = (quota as f64) / (period as f64);
        Some(cores)
    }

    pub(super) fn normalize_docker_state(status: ContainerStateStatusEnum) -> SandboxState {
        match status {
            ContainerStateStatusEnum::EMPTY => SandboxState::Unknown,
            ContainerStateStatusEnum::CREATED => SandboxState::Provisioning,
            ContainerStateStatusEnum::RUNNING => SandboxState::Running,
            ContainerStateStatusEnum::PAUSED => SandboxState::Paused,
            ContainerStateStatusEnum::RESTARTING => SandboxState::Starting,
            ContainerStateStatusEnum::REMOVING => SandboxState::Deleting,
            ContainerStateStatusEnum::EXITED => SandboxState::Stopped,
            ContainerStateStatusEnum::DEAD => SandboxState::Error,
        }
    }

    #[cfg(test)]
    mod tests {
        use bollard::models::HostConfig;
        use fabro_types::{
            RunSandboxInstance, RunSandboxRuntime, SandboxNetwork, SandboxNetworkPolicy,
            SandboxProviderKind,
        };

        use super::*;

        fn record() -> RunSandboxInstance {
            RunSandboxInstance {
                provider: SandboxProviderKind::Docker,
                image:    None,
                snapshot: None,
                runtime:  RunSandboxRuntime {
                    id:                "container-abc123".to_string(),
                    working_directory: "/workspace".to_string(),
                    repo_cloned:       Some(true),
                    clone_origin_url:  None,
                    clone_branch:      None,
                    workspace_root:    None,
                    repos_root:        None,
                    primary_repo_path: None,
                    primary_repo_link: None,
                },
            }
        }

        #[test]
        fn cpu_cores_divides_quota_by_period() {
            let host = HostConfig {
                cpu_quota: Some(200_000),
                cpu_period: Some(100_000),
                ..Default::default()
            };
            assert_eq!(docker_cpu_cores(&host), Some(2.0));
        }

        #[test]
        fn cpu_cores_returns_none_when_quota_missing() {
            let host = HostConfig {
                cpu_quota: None,
                cpu_period: Some(100_000),
                ..Default::default()
            };
            assert_eq!(docker_cpu_cores(&host), None);
        }

        #[test]
        fn cpu_cores_returns_none_when_period_zero() {
            let host = HostConfig {
                cpu_quota: Some(200_000),
                cpu_period: Some(0),
                ..Default::default()
            };
            assert_eq!(docker_cpu_cores(&host), None);
        }

        #[test]
        fn memory_bytes_zero_is_unset() {
            let inspect = ContainerInspectResponse {
                host_config: Some(HostConfig {
                    memory: Some(0),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let details = map_docker_inspect(&inspect, &record());
            assert_eq!(details.resources.memory_bytes, None);
        }

        #[test]
        fn memory_bytes_present_is_carried_through() {
            let inspect = ContainerInspectResponse {
                host_config: Some(HostConfig {
                    memory: Some(2 * 1024 * 1024 * 1024),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let details = map_docker_inspect(&inspect, &record());
            assert_eq!(details.resources.memory_bytes, Some(2_147_483_648));
        }

        #[test]
        fn network_mode_none_blocks_ingress_and_egress() {
            let inspect = ContainerInspectResponse {
                host_config: Some(HostConfig {
                    network_mode: Some("none".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let details = map_docker_inspect(&inspect, &record());
            assert_eq!(details.network.egress, SandboxNetworkPolicy::blocked());
            assert_eq!(details.network.ingress, SandboxNetworkPolicy::blocked());
        }

        #[test]
        fn non_none_network_mode_is_unknown() {
            let inspect = ContainerInspectResponse {
                host_config: Some(HostConfig {
                    network_mode: Some("bridge".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let details = map_docker_inspect(&inspect, &record());
            assert_eq!(details.network, SandboxNetwork::unknown());
        }

        #[test]
        fn record_identity_is_carried_through() {
            let inspect = ContainerInspectResponse {
                name: Some("/fabro-run-abc".to_string()),
                ..Default::default()
            };
            let details = map_docker_inspect(&inspect, &record());
            let runtime = details.sandbox.runtime;
            assert_eq!(runtime.id, "container-abc123");
            assert_eq!(runtime.working_directory, "/workspace");
        }

        #[test]
        fn inventory_identity_uses_native_id_and_display_name() {
            let inspect = ContainerInspectResponse {
                id: Some("container-abc123".to_string()),
                name: Some("/fabro-run-abc".to_string()),
                ..Default::default()
            };
            let info = docker_info_from_inspect(&inspect);
            assert_eq!(info.id, "container-abc123");
            assert_eq!(info.display_name.as_deref(), Some("fabro-run-abc"));
        }

        #[test]
        fn inventory_working_directory_defaults_to_fabro_workspace() {
            let inspect = ContainerInspectResponse::default();
            let info = docker_info_from_inspect(&inspect);
            assert_eq!(info.working_directory.as_deref(), Some("/workspace"));
        }

        #[test]
        fn empty_status_is_unknown() {
            assert_eq!(
                normalize_docker_state(ContainerStateStatusEnum::EMPTY),
                SandboxState::Unknown
            );
        }

        #[test]
        fn created_status_is_provisioning() {
            assert_eq!(
                normalize_docker_state(ContainerStateStatusEnum::CREATED),
                SandboxState::Provisioning
            );
        }

        #[test]
        fn running_status_is_running() {
            assert_eq!(
                normalize_docker_state(ContainerStateStatusEnum::RUNNING),
                SandboxState::Running
            );
        }

        #[test]
        fn paused_status_is_paused() {
            assert_eq!(
                normalize_docker_state(ContainerStateStatusEnum::PAUSED),
                SandboxState::Paused
            );
        }

        #[test]
        fn restarting_status_is_starting() {
            assert_eq!(
                normalize_docker_state(ContainerStateStatusEnum::RESTARTING),
                SandboxState::Starting
            );
        }

        #[test]
        fn removing_status_is_deleting() {
            assert_eq!(
                normalize_docker_state(ContainerStateStatusEnum::REMOVING),
                SandboxState::Deleting
            );
        }

        #[test]
        fn exited_status_is_stopped() {
            assert_eq!(
                normalize_docker_state(ContainerStateStatusEnum::EXITED),
                SandboxState::Stopped
            );
        }

        #[test]
        fn dead_status_is_error() {
            assert_eq!(
                normalize_docker_state(ContainerStateStatusEnum::DEAD),
                SandboxState::Error
            );
        }

        #[test]
        fn parse_timestamp_accepts_rfc3339() {
            let parsed = parse_rfc3339_utc("2026-05-09T12:00:00Z");
            assert!(parsed.is_some());
        }

        #[test]
        fn parse_timestamp_rejects_garbage() {
            assert!(parse_rfc3339_utc("not a date").is_none());
        }
    }
}

#[cfg(feature = "daytona")]
pub(crate) mod daytona {
    use std::collections::BTreeMap;

    use anyhow::{Context, Result, anyhow};
    use daytona_api_client::models::SandboxState as DaytonaState;
    use fabro_types::{
        RunSandboxInstance, SandboxDetails, SandboxInfo, SandboxNetwork, SandboxNetworkPolicy,
        SandboxProviderKind, SandboxResources, SandboxState, SandboxTimestamps,
    };

    use super::parse_rfc3339_utc;
    use crate::daytona::{DAYTONA_DASHBOARD_SANDBOXES_URL, DaytonaSandbox, WORKING_DIRECTORY};

    pub(super) async fn daytona_details(
        record: &RunSandboxInstance,
        daytona_api_key: Option<String>,
    ) -> Result<SandboxDetails> {
        let runtime = &record.runtime;
        let repo_cloned = runtime
            .repo_cloned
            .context("Daytona run sandbox missing clone metadata")?;

        let sandbox_handle = DaytonaSandbox::reconnect(
            &runtime.id,
            daytona_api_key,
            repo_cloned,
            runtime.working_directory.clone(),
            runtime.clone_origin_url.clone(),
            runtime.clone_branch.clone(),
        )
        .await
        .map_err(anyhow::Error::new)?;
        let sdk_sandbox = sandbox_handle
            .sandbox_handle()
            .ok_or_else(|| anyhow!("Daytona sandbox is not initialized after reconnect"))?;

        Ok(map_daytona_sandbox(sdk_sandbox, record))
    }

    pub(crate) fn daytona_info_from_sdk_sandbox(sandbox: &daytona_sdk::Sandbox) -> SandboxInfo {
        let fields = daytona_fields_from_sdk_sandbox(sandbox);
        SandboxInfo {
            provider:          SandboxProviderKind::Daytona,
            id:                sandbox.id.clone(),
            display_name:      Some(sandbox.name.clone()).filter(|name| !name.is_empty()),
            state:             fields.state,
            native_state:      fields.native_state,
            image:             None,
            snapshot:          sandbox.snapshot.clone(),
            region:            fields.region,
            web_url:           Some(daytona_dashboard_url(&sandbox.id)),
            working_directory: Some(WORKING_DIRECTORY.to_string()),
            resources:         fields.resources,
            network:           fields.network,
            labels:            fields.labels,
            timestamps:        fields.timestamps,
        }
    }

    pub(super) fn map_daytona_sandbox(
        sandbox: &daytona_sdk::Sandbox,
        record: &RunSandboxInstance,
    ) -> SandboxDetails {
        let fields = daytona_fields_from_sdk_sandbox(sandbox);
        SandboxDetails {
            sandbox:      RunSandboxInstance {
                snapshot: sandbox.snapshot.clone().or_else(|| record.snapshot.clone()),
                ..record.clone()
            },
            state:        fields.state,
            native_state: fields.native_state,
            region:       fields.region,
            web_url:      Some(daytona_dashboard_url(&sandbox.id)),
            resources:    fields.resources,
            network:      fields.network,
            labels:       fields.labels,
            timestamps:   fields.timestamps,
        }
    }

    struct DaytonaFields {
        state:        SandboxState,
        native_state: Option<String>,
        region:       Option<String>,
        resources:    SandboxResources,
        network:      SandboxNetwork,
        labels:       BTreeMap<String, String>,
        timestamps:   SandboxTimestamps,
    }

    fn daytona_fields_from_sdk_sandbox(sandbox: &daytona_sdk::Sandbox) -> DaytonaFields {
        let normalized_state = sandbox
            .state
            .map_or(SandboxState::Unknown, normalize_daytona_state);
        let native_state = sandbox.state.map(|state| state.to_string());

        let resources = SandboxResources {
            cpu_cores:    Some(sandbox.cpu),
            memory_bytes: gibibytes_to_bytes(sandbox.memory),
            disk_bytes:   gibibytes_to_bytes(sandbox.disk),
        };

        let labels: BTreeMap<String, String> = sandbox
            .labels
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();

        let target = sandbox.target.clone();
        let region = if target.is_empty() {
            None
        } else {
            Some(target)
        };

        DaytonaFields {
            state: normalized_state,
            native_state,
            region,
            resources,
            network: daytona_network(
                sandbox.network_block_all,
                sandbox.network_allow_list.as_deref(),
            ),
            labels,
            timestamps: SandboxTimestamps {
                created_at:       sandbox.created_at.as_deref().and_then(parse_rfc3339_utc),
                last_activity_at: sandbox.updated_at.as_deref().and_then(parse_rfc3339_utc),
            },
        }
    }

    /// The Daytona SDK reports CPU/memory/disk as floats in their respective
    /// SI units (cores, GiB, GiB). Convert mem/disk into bytes.
    fn gibibytes_to_bytes(value: f64) -> Option<u64> {
        if value <= 0.0 || !value.is_finite() {
            return None;
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "Daytona memory/disk values are well within u64 range and only need approximate byte counts."
        )]
        let bytes = (value * 1024.0 * 1024.0 * 1024.0) as u64;
        Some(bytes)
    }

    fn daytona_dashboard_url(sandbox_id: &str) -> String {
        format!("{DAYTONA_DASHBOARD_SANDBOXES_URL}?sandboxId={sandbox_id}")
    }

    fn daytona_network(
        network_block_all: bool,
        network_allow_list: Option<&str>,
    ) -> SandboxNetwork {
        let egress = if network_block_all {
            SandboxNetworkPolicy::blocked()
        } else {
            let cidrs = network_allow_list
                .into_iter()
                .flat_map(|allow_list| allow_list.split(','))
                .map(str::trim)
                .filter(|cidr| !cidr.is_empty());
            let cidrs: Vec<_> = cidrs.collect();
            if cidrs.is_empty() {
                SandboxNetworkPolicy::open()
            } else {
                SandboxNetworkPolicy::allow_cidrs(cidrs)
            }
        };

        SandboxNetwork {
            egress,
            ingress: SandboxNetworkPolicy::blocked(),
        }
    }

    pub(super) fn normalize_daytona_state(state: DaytonaState) -> SandboxState {
        match state {
            DaytonaState::Creating
            | DaytonaState::PendingBuild
            | DaytonaState::BuildingSnapshot
            | DaytonaState::PullingSnapshot => SandboxState::Provisioning,
            DaytonaState::Starting => SandboxState::Starting,
            DaytonaState::Started => SandboxState::Running,
            DaytonaState::Stopping | DaytonaState::Archiving => SandboxState::Stopping,
            DaytonaState::Stopped => SandboxState::Stopped,
            DaytonaState::Restoring => SandboxState::Restoring,
            DaytonaState::Resizing => SandboxState::Resizing,
            DaytonaState::Archived => SandboxState::Archived,
            DaytonaState::Destroying => SandboxState::Deleting,
            DaytonaState::Destroyed => SandboxState::Deleted,
            DaytonaState::Error | DaytonaState::BuildFailed => SandboxState::Error,
            DaytonaState::Unknown => SandboxState::Unknown,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn started_normalizes_to_running() {
            assert_eq!(
                normalize_daytona_state(DaytonaState::Started),
                SandboxState::Running
            );
        }

        #[test]
        fn creating_normalizes_to_provisioning() {
            assert_eq!(
                normalize_daytona_state(DaytonaState::Creating),
                SandboxState::Provisioning
            );
        }

        #[test]
        fn building_snapshot_normalizes_to_provisioning() {
            assert_eq!(
                normalize_daytona_state(DaytonaState::BuildingSnapshot),
                SandboxState::Provisioning
            );
        }

        #[test]
        fn stopped_normalizes_to_stopped() {
            assert_eq!(
                normalize_daytona_state(DaytonaState::Stopped),
                SandboxState::Stopped
            );
        }

        #[test]
        fn archived_normalizes_to_archived() {
            assert_eq!(
                normalize_daytona_state(DaytonaState::Archived),
                SandboxState::Archived
            );
        }

        #[test]
        fn destroyed_normalizes_to_deleted() {
            assert_eq!(
                normalize_daytona_state(DaytonaState::Destroyed),
                SandboxState::Deleted
            );
        }

        #[test]
        fn build_failed_normalizes_to_error() {
            assert_eq!(
                normalize_daytona_state(DaytonaState::BuildFailed),
                SandboxState::Error
            );
        }

        #[test]
        fn unknown_normalizes_to_unknown() {
            assert_eq!(
                normalize_daytona_state(DaytonaState::Unknown),
                SandboxState::Unknown
            );
        }

        #[test]
        fn gibibytes_to_bytes_converts_positive_values() {
            assert_eq!(gibibytes_to_bytes(2.0), Some(2 * 1024 * 1024 * 1024));
        }

        #[test]
        fn gibibytes_to_bytes_returns_none_for_zero() {
            assert_eq!(gibibytes_to_bytes(0.0), None);
        }

        #[test]
        fn daytona_dashboard_url_uses_sandbox_id_query_param() {
            assert_eq!(
                daytona_dashboard_url("ad65029a-2d01-421e-8936-49451653fcd9"),
                "https://app.daytona.io/dashboard/sandboxes?sandboxId=ad65029a-2d01-421e-8936-49451653fcd9",
            );
        }

        #[test]
        fn network_block_all_blocks_egress_and_ingress() {
            let network = daytona_network(true, Some("10.0.0.0/8"));
            assert_eq!(network.egress, SandboxNetworkPolicy::blocked());
            assert_eq!(network.ingress, SandboxNetworkPolicy::blocked());
        }

        #[test]
        fn network_allow_list_maps_to_cidr_allow_list_and_blocks_ingress() {
            let network = daytona_network(false, Some("10.0.0.0/8, 192.168.0.0/16 "));
            assert_eq!(
                network.egress,
                SandboxNetworkPolicy::allow_cidrs(["10.0.0.0/8", "192.168.0.0/16"])
            );
            assert_eq!(network.ingress, SandboxNetworkPolicy::blocked());
        }

        #[test]
        fn empty_network_allow_list_is_open_egress_and_blocked_ingress() {
            let network = daytona_network(false, Some(" , "));
            assert_eq!(network.egress, SandboxNetworkPolicy::open());
            assert_eq!(network.ingress, SandboxNetworkPolicy::blocked());
        }

        #[test]
        fn default_daytona_network_is_open_egress_and_blocked_ingress() {
            let network = daytona_network(false, None);
            assert_eq!(network.egress, SandboxNetworkPolicy::open());
            assert_eq!(network.ingress, SandboxNetworkPolicy::blocked());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_details_returns_running_with_no_metadata() {
        let record = RunSandboxInstance {
            provider: SandboxProviderKind::Local,
            image:    None,
            snapshot: None,
            runtime:  fabro_types::RunSandboxRuntime {
                id:                "local:01JNQVR7M0EJ5GKAT2SC4ERS1Z".to_string(),
                working_directory: "/Users/client/project".to_string(),
                repo_cloned:       None,
                clone_origin_url:  None,
                clone_branch:      None,
                workspace_root:    None,
                repos_root:        None,
                primary_repo_path: None,
                primary_repo_link: None,
            },
        };
        let details = local_details(&record);
        assert_eq!(details.sandbox.provider, SandboxProviderKind::Local);
        assert_eq!(details.state, SandboxState::Running);
        let runtime = &details.sandbox.runtime;
        assert_eq!(runtime.id, "local:01JNQVR7M0EJ5GKAT2SC4ERS1Z");
        assert_eq!(runtime.working_directory, "/Users/client/project");
        assert!(details.region.is_none());
        assert!(details.sandbox.image.is_none());
        assert!(details.labels.is_empty());
        assert_eq!(details.resources, SandboxResources::default());
        assert_eq!(details.network, SandboxNetwork::unknown());
        assert_eq!(details.timestamps, SandboxTimestamps::default());
    }
}
