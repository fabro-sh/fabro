use std::collections::BTreeMap;

use anyhow::Result;
#[cfg(any(feature = "docker", feature = "daytona"))]
use chrono::{DateTime, Utc};
use fabro_types::{
    RunId, RunSandbox, SandboxDetails, SandboxProvider, SandboxResources, SandboxState,
    SandboxTimestamps,
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
    record: &RunSandbox,
    daytona_api_key: Option<String>,
    daytona_organization_id: Option<String>,
    run_id: Option<RunId>,
) -> Result<SandboxDetails> {
    match record.provider {
        SandboxProvider::Local => Ok(local_details(record)),
        #[cfg(feature = "docker")]
        SandboxProvider::Docker => docker::docker_details(record, run_id).await,
        #[cfg(not(feature = "docker"))]
        SandboxProvider::Docker => Err(anyhow::anyhow!(
            "Sandbox provider '{}' has no details implementation",
            record.provider
        )),
        #[cfg(feature = "daytona")]
        SandboxProvider::Daytona => daytona::daytona_details(record, daytona_api_key).await,
        #[cfg(not(feature = "daytona"))]
        SandboxProvider::Daytona => Err(anyhow::anyhow!(
            "Sandbox provider '{}' has no details implementation",
            record.provider
        )),
    }
}

fn local_details(record: &RunSandbox) -> SandboxDetails {
    SandboxDetails {
        sandbox:      record.clone(),
        state:        SandboxState::Running,
        native_state: None,
        region:       None,
        resources:    SandboxResources::default(),
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
mod docker {
    use std::collections::BTreeMap;

    use anyhow::{Context, Result, anyhow};
    use bollard::Docker;
    use bollard::container::InspectContainerOptions;
    use bollard::models::{ContainerInspectResponse, ContainerStateStatusEnum, HostConfig};
    use fabro_types::{
        RunId, RunSandbox, SandboxDetails, SandboxResources, SandboxState, SandboxTimestamps,
    };

    use super::parse_rfc3339_utc;

    pub(super) async fn docker_details(
        record: &RunSandbox,
        _run_id: Option<RunId>,
    ) -> Result<SandboxDetails> {
        let docker =
            Docker::connect_with_local_defaults().context("Failed to connect to Docker daemon")?;
        let runtime = record
            .runtime
            .as_ref()
            .context("Docker run sandbox missing runtime metadata")?;
        let inspect = docker
            .inspect_container(&runtime.id, None::<InspectContainerOptions>)
            .await
            .map_err(|err| anyhow!("Failed to inspect Docker container '{}': {err}", runtime.id))?;
        Ok(map_docker_inspect(inspect, record))
    }

    fn map_docker_inspect(
        inspect: ContainerInspectResponse,
        record: &RunSandbox,
    ) -> SandboxDetails {
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

        let labels: BTreeMap<String, String> = inspect
            .config
            .and_then(|config| config.labels)
            .map(|map| map.into_iter().collect())
            .unwrap_or_default();

        let image = inspect.image;

        let created_at = inspect.created.as_deref().and_then(parse_rfc3339_utc);

        SandboxDetails {
            sandbox: RunSandbox {
                image: image.or_else(|| record.image.clone()),
                ..record.clone()
            },
            state: normalized_state,
            native_state,
            region: None,
            resources,
            labels,
            timestamps: SandboxTimestamps {
                created_at,
                last_activity_at: None,
            },
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
        use fabro_types::{RunSandbox, RunSandboxRuntime, SandboxProvider};

        use super::*;

        fn record() -> RunSandbox {
            RunSandbox {
                provider: SandboxProvider::Docker,
                image:    None,
                snapshot: None,
                runtime:  Some(RunSandboxRuntime {
                    id:                "container-abc123".to_string(),
                    working_directory: "/workspace".to_string(),
                    repo_cloned:       Some(true),
                    clone_origin_url:  None,
                    clone_branch:      None,
                }),
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
            let details = map_docker_inspect(inspect, &record());
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
            let details = map_docker_inspect(inspect, &record());
            assert_eq!(details.resources.memory_bytes, Some(2_147_483_648));
        }

        #[test]
        fn record_identity_is_carried_through() {
            let inspect = ContainerInspectResponse {
                name: Some("/fabro-run-abc".to_string()),
                ..Default::default()
            };
            let details = map_docker_inspect(inspect, &record());
            let runtime = details.sandbox.runtime.expect("runtime");
            assert_eq!(runtime.id, "container-abc123");
            assert_eq!(runtime.working_directory, "/workspace");
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
mod daytona {
    use std::collections::BTreeMap;

    use anyhow::{Context, Result, anyhow};
    use daytona_api_client::models::SandboxState as DaytonaState;
    use fabro_types::{
        RunSandbox, SandboxDetails, SandboxResources, SandboxState, SandboxTimestamps,
    };

    use super::parse_rfc3339_utc;
    use crate::daytona::DaytonaSandbox;

    pub(super) async fn daytona_details(
        record: &RunSandbox,
        daytona_api_key: Option<String>,
    ) -> Result<SandboxDetails> {
        let runtime = record
            .runtime
            .as_ref()
            .context("Daytona run sandbox missing runtime metadata")?;
        let repo_cloned = runtime
            .repo_cloned
            .context("Daytona run sandbox missing clone metadata")?;

        let sandbox_handle = DaytonaSandbox::reconnect(
            &runtime.id,
            daytona_api_key,
            repo_cloned,
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

    fn map_daytona_sandbox(sandbox: &daytona_sdk::Sandbox, record: &RunSandbox) -> SandboxDetails {
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

        SandboxDetails {
            sandbox: RunSandbox {
                snapshot: sandbox.snapshot.clone().or_else(|| record.snapshot.clone()),
                ..record.clone()
            },
            state: normalized_state,
            native_state,
            region,
            resources,
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_details_returns_running_with_no_metadata() {
        let record = RunSandbox {
            provider: SandboxProvider::Local,
            image:    None,
            snapshot: None,
            runtime:  Some(fabro_types::RunSandboxRuntime {
                id:                "local:01JNQVR7M0EJ5GKAT2SC4ERS1Z".to_string(),
                working_directory: "/Users/client/project".to_string(),
                repo_cloned:       None,
                clone_origin_url:  None,
                clone_branch:      None,
            }),
        };
        let details = local_details(&record);
        assert_eq!(details.sandbox.provider, SandboxProvider::Local);
        assert_eq!(details.state, SandboxState::Running);
        let runtime = details.sandbox.runtime.as_ref().unwrap();
        assert_eq!(runtime.id, "local:01JNQVR7M0EJ5GKAT2SC4ERS1Z");
        assert_eq!(runtime.working_directory, "/Users/client/project");
        assert!(details.region.is_none());
        assert!(details.sandbox.image.is_none());
        assert!(details.labels.is_empty());
        assert_eq!(details.resources, SandboxResources::default());
        assert_eq!(details.timestamps, SandboxTimestamps::default());
    }
}
