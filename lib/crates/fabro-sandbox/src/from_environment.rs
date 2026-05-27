//! Convert resolved [`RunEnvironmentSettings`] into runtime sandbox configs.
//!
//! These mappings are consumed by both the workflow run-start path and the
//! server preflight path, so they live here next to their destination types.

#[cfg(feature = "daytona")]
use fabro_types::settings::run::DockerfileSource as ResolvedDockerfileSource;
use fabro_types::settings::run::{EnvironmentNetworkMode, RunEnvironmentSettings};

#[cfg(feature = "daytona")]
use crate::config::{
    DaytonaNetwork, DaytonaSnapshotSettings, DaytonaVolumeMount,
    DockerfileSource as SandboxDockerfileSource,
};
#[cfg(feature = "daytona")]
use crate::daytona::DaytonaConfig;
#[cfg(feature = "docker")]
use crate::docker::DockerSandboxOptions;

#[cfg(feature = "daytona")]
#[must_use]
pub fn daytona_config_from_environment(
    settings: &RunEnvironmentSettings,
    skip_clone: bool,
) -> DaytonaConfig {
    DaytonaConfig {
        auto_stop_interval: settings
            .lifecycle
            .auto_stop
            .map(|duration| duration_to_minutes_i32(duration.as_std())),
        labels: (!settings.labels.is_empty()).then(|| settings.labels.clone()),
        volumes: settings
            .volumes
            .iter()
            .map(|volume| DaytonaVolumeMount {
                volume_id:  volume.id.clone(),
                mount_path: volume.mount_path.clone(),
                subpath:    volume.subpath.clone(),
            })
            .collect(),
        snapshot: settings
            .image
            .dockerfile
            .as_ref()
            .map(|dockerfile| DaytonaSnapshotSettings {
                cpu:        settings.resources.cpu,
                memory:     settings
                    .resources
                    .memory
                    .map(|size| size_to_gb_i32(size.as_bytes())),
                disk:       settings
                    .resources
                    .disk
                    .map(|size| size_to_gb_i32(size.as_bytes())),
                dockerfile: Some(match dockerfile {
                    ResolvedDockerfileSource::Inline(text) => {
                        SandboxDockerfileSource::Inline(text.clone())
                    }
                    ResolvedDockerfileSource::Path { path } => {
                        SandboxDockerfileSource::Path { path: path.clone() }
                    }
                }),
            }),
        network: Some(match settings.network.mode {
            EnvironmentNetworkMode::Block => DaytonaNetwork::Block,
            EnvironmentNetworkMode::AllowAll => DaytonaNetwork::AllowAll,
            EnvironmentNetworkMode::CidrAllowList => {
                DaytonaNetwork::AllowList(settings.network.allow.clone())
            }
        }),
        skip_clone,
    }
}

#[cfg(feature = "docker")]
#[must_use]
pub fn docker_config_from_environment(
    settings: &RunEnvironmentSettings,
    skip_clone: bool,
) -> DockerSandboxOptions {
    let mut env_vars = settings
        .resolve_env(process_env_var)
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>();
    env_vars.sort();
    let default_options = DockerSandboxOptions::default();

    DockerSandboxOptions {
        image: settings
            .image
            .docker
            .clone()
            .unwrap_or(default_options.image),
        network_mode: match settings.network.mode {
            EnvironmentNetworkMode::Block => Some("none".to_string()),
            EnvironmentNetworkMode::AllowAll | EnvironmentNetworkMode::CidrAllowList => {
                default_options.network_mode
            }
        },
        memory_limit: settings
            .resources
            .memory
            .and_then(|size| i64::try_from(size.as_bytes()).ok()),
        cpu_quota: settings
            .resources
            .cpu
            .map(|cpu| i64::from(cpu).saturating_mul(100_000)),
        env_vars,
        skip_clone,
        ..DockerSandboxOptions::default()
    }
}

#[cfg(feature = "docker")]
#[expect(
    clippy::disallowed_methods,
    reason = "Environment interpolation owns a process-env lookup facade for {{ env.* }} values."
)]
fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

#[cfg(feature = "daytona")]
fn duration_to_minutes_i32(duration: std::time::Duration) -> i32 {
    let minutes = duration.as_secs() / 60;
    i32::try_from(minutes).unwrap_or(i32::MAX)
}

#[cfg(feature = "daytona")]
fn size_to_gb_i32(bytes: u64) -> i32 {
    let gb = bytes / 1_000_000_000;
    i32::try_from(gb).unwrap_or(i32::MAX)
}
